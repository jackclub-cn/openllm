use super::*;

pub(in crate::proxy) fn target_upstream_endpoint<'a>(
    target: &RouteTarget,
    request_endpoint: &'a str,
) -> Option<&'a str> {
    let declared = supported_endpoint_list(target.supported_endpoints.as_deref());
    let declared = (!declared.is_empty()).then_some(declared);
    crate::registry::upstream_endpoint_for(
        &target.provider_type,
        declared.as_deref(),
        request_endpoint,
    )
}

pub(in crate::proxy) fn target_supports_endpoint(
    target: &RouteTarget,
    request_endpoint: &str,
) -> bool {
    let declared = supported_endpoint_list(target.supported_endpoints.as_deref());
    let declared = (!declared.is_empty()).then_some(declared);
    crate::registry::endpoint_served(&target.provider_type, declared.as_deref(), request_endpoint)
}

pub(in crate::proxy) fn supported_endpoint_list(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

pub(in crate::proxy) fn filter_targets_for_endpoint(
    targets: Vec<RouteTarget>,
    request_endpoint: &str,
) -> Vec<RouteTarget> {
    targets
        .into_iter()
        .filter(|target| target_supports_endpoint(target, request_endpoint))
        .collect()
}

pub(in crate::proxy) async fn resolve_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
) -> AppResult<ResolvedRoute> {
    if let Some(route) = find_explicit_route(state, model).await? {
        let targets = filter_targets_for_endpoint(load_targets(state, route.id).await?, endpoint);
        if targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "model '{model}' has no enabled route target that supports endpoint '{endpoint}'"
            )));
        }
        let pairs = targets
            .iter()
            .map(|target| (target.provider_id, target.upstream_model.clone()))
            .collect::<Vec<_>>();
        let mut barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        crate::registry::apply_route_capability_overrides(
            &mut barrel,
            route.context_override,
            route.output_override,
        );
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
            route_name: Some(route.name),
            strategy: route.strategy,
            targets,
            barrel: Some(barrel),
            all_targets_busy_wait_seconds: route.all_targets_busy_wait_seconds,
            retry_jitter_ms: route.retry_jitter_ms,
            max_waiting_requests: route.max_waiting_requests,
            keepalive_during_wait: route.keepalive_during_wait != 0,
        });
    }

    let targets = find_prefixed_targets(state, model, endpoint).await?;
    if targets.is_empty() {
        return Err(AppError::BadRequest(format!(
            "model '{model}' is not available for endpoint '{endpoint}'"
        )));
    }
    let pairs = targets
        .iter()
        .map(|target| (target.provider_id, target.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
    Ok(ResolvedRoute {
        route_id: None,
        route_name: None,
        strategy: "priority".to_string(),
        targets,
        barrel: Some(barrel),
        all_targets_busy_wait_seconds: 0,
        retry_jitter_ms: 250,
        max_waiting_requests: 32,
        keepalive_during_wait: true,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::proxy) async fn resolve_route_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
) -> AppResult<ResolvedRoute> {
    match resolve_route(state, model, endpoint).await {
        Ok(route) => {
            if let Some(capabilities) = route
                .barrel
                .as_ref()
                .and_then(|barrel| barrel.capabilities.as_ref())
            {
                let _ = state.activity.context_window(
                    request_id,
                    capabilities
                        .total_context_tokens
                        .or(capabilities.context_limit),
                    capabilities.input_limit,
                );
            }
            Ok(route)
        }
        Err(error) => {
            let status_code = match &error {
                AppError::BadRequest(_) => 400,
                AppError::Unauthorized(_) => 401,
                AppError::Forbidden(_) => 403,
                AppError::NotFound(_) => 404,
                AppError::Conflict(_) => 409,
                AppError::TooManyRequests(_) => 429,
                AppError::Upstream(_) => 502,
                AppError::UpstreamStatus { status, .. } => status.as_u16() as i64,
                AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => 500,
            };
            let message = error.to_string();
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

pub(in crate::proxy) async fn find_explicit_route(
    state: &AppState,
    model: &str,
) -> AppResult<Option<Route>> {
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, model_pattern,
               CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy,
               enabled, context_override, output_override,
               all_targets_busy_wait_seconds, retry_jitter_ms,
               max_waiting_requests, keepalive_during_wait,
               created_at, updated_at
        FROM routes
        WHERE enabled = 1
        ORDER BY
            CASE WHEN instr(model_pattern, '*') = 0 AND instr(model_pattern, '?') = 0 THEN 0 ELSE 1 END,
            length(model_pattern) DESC,
            id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for route in routes {
        let Ok(glob) = Glob::new(&route.model_pattern) else {
            tracing::warn!(pattern = %route.model_pattern, "ignoring invalid route pattern");
            continue;
        };
        if glob.compile_matcher().is_match(model) {
            return Ok(Some(route));
        }
    }

    Ok(None)
}

pub(in crate::proxy) async fn find_prefixed_targets(
    state: &AppState,
    model: &str,
    endpoint: &str,
) -> AppResult<Vec<RouteTarget>> {
    let prefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               COALESCE(pm.context_override, pm.context_limit) AS context_limit,
               COALESCE(pm.input_override, pm.input_limit) AS input_limit,
               p.tool_search_supported,
               p.timeout_seconds, p.cooldown_seconds,
               p.max_concurrency, p.queue_timeout_seconds,
               pm.max_concurrency AS model_max_concurrency,
               pm.queue_timeout_seconds AS model_queue_timeout_seconds,
               p.last_test_ok AS provider_health,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix <> ''
          AND substr(?, 1, length(p.model_prefix)) = p.model_prefix
          AND substr(?, length(p.model_prefix) + 1) = pm.model_name
        ORDER BY p.id, pm.model_name
        "#,
    )
    .bind(model)
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    let prefixed = filter_targets_for_endpoint(prefixed, endpoint);
    if !prefixed.is_empty() {
        return Ok(prefixed);
    }

    let unprefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               COALESCE(pm.context_override, pm.context_limit) AS context_limit,
               COALESCE(pm.input_override, pm.input_limit) AS input_limit,
               p.tool_search_supported,
               p.timeout_seconds, p.cooldown_seconds,
               p.max_concurrency, p.queue_timeout_seconds,
               pm.max_concurrency AS model_max_concurrency,
               pm.queue_timeout_seconds AS model_queue_timeout_seconds,
               p.last_test_ok AS provider_health,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix = ''
          AND pm.model_name = ?
        ORDER BY p.id
        LIMIT 2
        "#,
    )
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    let unprefixed = filter_targets_for_endpoint(unprefixed, endpoint);
    if unprefixed.len() > 1 {
        return Err(AppError::Conflict(format!(
            "model '{model}' exists on multiple providers; configure a model prefix or an explicit route"
        )));
    }
    if unprefixed.is_empty() {
        return Err(AppError::NotFound(format!(
            "no enabled provider model matches '{model}' for endpoint '{endpoint}'"
        )));
    }
    Ok(unprefixed)
}

pub(in crate::proxy) async fn load_targets(
    state: &AppState,
    route_id: i64,
) -> AppResult<Vec<RouteTarget>> {
    Ok(sqlx::query_as::<_, RouteTarget>(
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
               p.tool_search_supported,
               p.timeout_seconds, p.cooldown_seconds,
               p.max_concurrency, p.queue_timeout_seconds,
               pm.max_concurrency AS model_max_concurrency,
               pm.queue_timeout_seconds AS model_queue_timeout_seconds,
               p.last_test_ok AS provider_health,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
        WHERE rt.route_id = ? AND rt.enabled = 1 AND p.enabled = 1
          AND NOT EXISTS (
              SELECT 1 FROM provider_models pm
              WHERE pm.provider_id = rt.provider_id
                AND pm.model_name = rt.upstream_model
                AND pm.enabled = 0
          )
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route_id)
    .fetch_all(&state.pool)
    .await?)
}
