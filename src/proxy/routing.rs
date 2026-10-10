use super::*;

pub(super) fn target_upstream_endpoint<'a>(
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

pub(super) fn target_supports_endpoint(target: &RouteTarget, request_endpoint: &str) -> bool {
    let declared = supported_endpoint_list(target.supported_endpoints.as_deref());
    let declared = (!declared.is_empty()).then_some(declared);
    crate::registry::endpoint_served(&target.provider_type, declared.as_deref(), request_endpoint)
}

pub(super) fn supported_endpoint_list(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

pub(super) fn filter_targets_for_endpoint(
    targets: Vec<RouteTarget>,
    request_endpoint: &str,
) -> Vec<RouteTarget> {
    targets
        .into_iter()
        .filter(|target| target_supports_endpoint(target, request_endpoint))
        .collect()
}

#[cfg(test)]
pub(super) async fn resolve_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
) -> AppResult<ResolvedRoute> {
    resolve_route_with_patterns(state, model, endpoint, None).await
}

pub(super) async fn resolve_route_with_patterns(
    state: &AppState,
    model: &str,
    endpoint: &str,
    model_patterns: Option<&[String]>,
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
        let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
            strategy: route.strategy,
            targets,
            barrel: Some(barrel),
        });
    }

    if let Some(strategy) = crate::registry::auto_model_strategy(model) {
        let targets = find_auto_targets(state, endpoint, model_patterns).await?;
        return Ok(ResolvedRoute {
            route_id: None,
            strategy: strategy.to_string(),
            targets,
            // Auto spans heterogeneous models. A common barrel would clamp
            // every request to the smallest model in the catalog, defeating
            // the purpose of automatic selection.
            barrel: None,
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
        strategy: "priority".to_string(),
        targets,
        barrel: Some(barrel),
    })
}

pub async fn diagnose_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let session_id = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(SESSION_ID_MAX_CHARS).collect::<String>());
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, model_pattern,
               CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy,
               enabled, created_at, updated_at
        FROM routes
        ORDER BY
            CASE WHEN instr(model_pattern, '*') = 0 AND instr(model_pattern, '?') = 0 THEN 0 ELSE 1 END,
            length(model_pattern) DESC,
            id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut disabled_match = None;
    for route in routes {
        let Ok(glob) = Glob::new(&route.model_pattern) else {
            continue;
        };
        if !glob.compile_matcher().is_match(model) {
            continue;
        }
        if route.enabled != 0 {
            return diagnose_explicit_route(
                state,
                model,
                endpoint,
                route,
                true,
                session_id.as_deref(),
            )
            .await;
        }
        disabled_match.get_or_insert(route);
    }
    if let Some(strategy) = crate::registry::auto_model_strategy(model) {
        return diagnose_auto_route(state, model, endpoint, strategy, session_id.as_deref()).await;
    }
    let direct = diagnose_direct_route(state, model, endpoint, session_id.as_deref()).await?;
    if direct.matched {
        return Ok(direct);
    }
    if let Some(route) = disabled_match {
        return diagnose_explicit_route(
            state,
            model,
            endpoint,
            route,
            false,
            session_id.as_deref(),
        )
        .await;
    }
    Ok(direct)
}

async fn diagnose_auto_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    strategy: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let rows = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, 0 AS target_id,
               pm.model_name AS upstream_model,
               100 AS target_weight, 0 AS target_priority,
               1 AS target_enabled, p.enabled AS provider_enabled,
               1 AS model_exists, pm.enabled AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               p.last_test_ok AS provider_health
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
        ORDER BY p.id, pm.model_name
        "#,
    )
    .fetch_all(&state.pool)
    .await?;
    let diagnosed = rows
        .iter()
        .map(|row| row.diagnose(endpoint, true))
        .collect::<Vec<_>>();
    let eligible = diagnosed.iter().filter(|(_, eligible)| *eligible).count();
    let resolved = eligible > 0;
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(state, None, strategy, &rows, &diagnosed, session_id).await?
    } else {
        None
    };
    Ok(RouteDiagnoseView {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        matched: true,
        resolved,
        match_type: "auto".to_string(),
        route_id: None,
        route_name: Some("Auto".to_string()),
        strategy: Some(strategy.to_string()),
        message: if resolved {
            format!("{eligible} auto target(s) can serve {endpoint}")
        } else {
            format!("auto has no eligible target for {endpoint}")
        },
        barrel: None,
        barrel_incomplete: false,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

pub(super) async fn diagnose_explicit_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    route: Route,
    route_enabled: bool,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let rows = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, rt.id AS target_id, rt.upstream_model,
               rt.weight AS target_weight, rt.priority AS target_priority,
               rt.enabled AS target_enabled,
               p.enabled AS provider_enabled,
               CASE WHEN pm.model_name IS NULL THEN 0 ELSE 1 END AS model_exists,
               COALESCE(pm.enabled, 1) AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               p.last_test_ok AS provider_health
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
        WHERE rt.route_id = ?
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route.id)
    .fetch_all(&state.pool)
    .await?;

    let diagnosed = rows
        .iter()
        .map(|row| row.diagnose(endpoint, route_enabled))
        .collect::<Vec<_>>();
    let eligible_pairs = rows
        .iter()
        .zip(&diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| (row.provider_id, row.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &eligible_pairs).await?;
    let resolved = route_enabled && !eligible_pairs.is_empty();
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(
            state,
            Some(route.id),
            &route.strategy,
            &rows,
            &diagnosed,
            session_id,
        )
        .await?
    } else {
        None
    };
    let message = if !route_enabled {
        format!("route '{}' is disabled", route.name)
    } else if resolved {
        format!("{} target(s) can serve {}", eligible_pairs.len(), endpoint)
    } else {
        format!(
            "route '{}' has no eligible target for {endpoint}",
            route.name
        )
    };

    Ok(RouteDiagnoseView {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        matched: true,
        resolved,
        match_type: "explicit_route".to_string(),
        route_id: Some(route.id),
        route_name: Some(route.name),
        strategy: Some(route.strategy),
        message,
        barrel: barrel.capabilities,
        barrel_incomplete: barrel.incomplete,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

pub(super) async fn diagnose_direct_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let prefixed = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, p.id AS target_id, pm.model_name AS upstream_model,
               100 AS target_weight, 0 AS target_priority,
               1 AS target_enabled,
               p.enabled AS provider_enabled,
               1 AS model_exists,
               pm.enabled AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               p.last_test_ok AS provider_health
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id
        WHERE p.model_prefix <> ''
          AND substr(?, 1, length(p.model_prefix)) = p.model_prefix
          AND substr(?, length(p.model_prefix) + 1) = pm.model_name
        ORDER BY p.id, pm.model_name
        "#,
    )
    .bind(model)
    .bind(model)
    .fetch_all(&state.pool)
    .await?;
    let prefixed_diagnosed = prefixed
        .iter()
        .map(|row| row.diagnose(endpoint, true))
        .collect::<Vec<_>>();
    if prefixed_diagnosed.iter().any(|(_, eligible)| *eligible) {
        return build_direct_diagnosis(
            state,
            model,
            endpoint,
            "prefix",
            prefixed,
            prefixed_diagnosed,
            session_id,
        )
        .await;
    }

    let unprefixed = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, p.id AS target_id, pm.model_name AS upstream_model,
               100 AS target_weight, 0 AS target_priority,
               1 AS target_enabled,
               p.enabled AS provider_enabled,
               1 AS model_exists,
               pm.enabled AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override,
               p.last_test_ok AS provider_health
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id
        WHERE p.model_prefix = '' AND pm.model_name = ?
        ORDER BY p.id
        "#,
    )
    .bind(model)
    .fetch_all(&state.pool)
    .await?;
    let unprefixed_diagnosed = unprefixed
        .iter()
        .map(|row| row.diagnose(endpoint, true))
        .collect::<Vec<_>>();
    if !unprefixed.is_empty() {
        return build_direct_diagnosis(
            state,
            model,
            endpoint,
            "direct",
            unprefixed,
            unprefixed_diagnosed,
            session_id,
        )
        .await;
    }

    let match_type = if prefixed.is_empty() {
        "none"
    } else {
        "prefix"
    };
    build_direct_diagnosis(
        state,
        model,
        endpoint,
        match_type,
        prefixed,
        prefixed_diagnosed,
        session_id,
    )
    .await
}

pub(super) async fn build_direct_diagnosis(
    state: &AppState,
    model: &str,
    endpoint: &str,
    match_type: &str,
    rows: Vec<DiagnosticTargetRow>,
    diagnosed: Vec<(RouteDiagnoseTarget, bool)>,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let eligible_pairs = rows
        .iter()
        .zip(&diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| (row.provider_id, row.upstream_model.clone()))
        .collect::<Vec<_>>();
    let matched = !rows.is_empty();
    let conflict = match_type == "direct" && eligible_pairs.len() > 1;
    let resolved = if match_type == "prefix" {
        !eligible_pairs.is_empty()
    } else {
        eligible_pairs.len() == 1
    };
    let effective_match_type = if conflict { "conflict" } else { match_type };
    let message = if !matched {
        format!("no enabled or disabled provider model matches '{model}'")
    } else if conflict {
        format!("model '{model}' exists on multiple providers; add a prefix or explicit route")
    } else if resolved {
        format!("direct model match can serve {endpoint}")
    } else {
        format!("model '{model}' exists but no target can serve {endpoint}")
    };
    let barrel = crate::registry::barrel_for_targets(&state.pool, &eligible_pairs).await?;
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(
            state,
            None,
            RouteStrategy::Priority.as_str(),
            &rows,
            &diagnosed,
            session_id,
        )
        .await?
    } else {
        None
    };

    Ok(RouteDiagnoseView {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        matched,
        resolved,
        match_type: effective_match_type.to_string(),
        route_id: None,
        route_name: None,
        strategy: None,
        message,
        barrel: barrel.capabilities,
        barrel_incomplete: barrel.incomplete,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct DiagnosticTargetRow {
    provider_id: i64,
    provider_name: String,
    provider_type: String,
    target_id: i64,
    upstream_model: String,
    target_weight: i64,
    target_priority: i64,
    target_enabled: i64,
    provider_enabled: i64,
    model_exists: i64,
    model_enabled: i64,
    supported_endpoints: Option<String>,
    cost: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
    provider_health: Option<i64>,
}

impl DiagnosticTargetRow {
    fn diagnose(&self, endpoint: &str, route_enabled: bool) -> (RouteDiagnoseTarget, bool) {
        let reason = if !route_enabled {
            Some("route is disabled".to_string())
        } else if self.target_enabled == 0 {
            Some("route target is disabled".to_string())
        } else if self.provider_enabled == 0 {
            Some("provider is disabled".to_string())
        } else if self.model_exists != 0 && self.model_enabled == 0 {
            Some("model is disabled".to_string())
        } else {
            let declared = supported_endpoint_list(self.supported_endpoints.as_deref());
            let declared = (!declared.is_empty()).then_some(declared);
            if crate::registry::upstream_endpoint_for(
                &self.provider_type,
                declared.as_deref(),
                endpoint,
            )
            .is_none()
            {
                Some("provider does not support this endpoint".to_string())
            } else if !crate::registry::endpoint_served(
                &self.provider_type,
                declared.as_deref(),
                endpoint,
            ) {
                Some("model does not declare support for this endpoint".to_string())
            } else {
                None
            }
        };
        let eligible = reason.is_none();
        (
            RouteDiagnoseTarget {
                provider_id: self.provider_id,
                provider_name: self.provider_name.clone(),
                provider_type: self.provider_type.clone(),
                upstream_model: self.upstream_model.clone(),
                eligible,
                reason: reason.unwrap_or_else(|| "eligible".to_string()),
                supported_endpoints: supported_endpoint_list(self.supported_endpoints.as_deref()),
                provider_health: self.provider_health.map(|value| value != 0),
            },
            eligible,
        )
    }
}

pub(super) async fn diagnostic_runtime_targets(
    state: &AppState,
    route_id: Option<i64>,
    strategy: &str,
    rows: &[DiagnosticTargetRow],
    diagnosed: &[(RouteDiagnoseTarget, bool)],
    session_id: Option<&str>,
) -> AppResult<Option<Vec<RouteDiagnoseRuntimeTarget>>> {
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let targets = rows
        .iter()
        .zip(diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| RouteTarget {
            id: row.target_id,
            route_id,
            provider_id: row.provider_id,
            provider_name: row.provider_name.clone(),
            provider_type: row.provider_type.clone(),
            base_url: String::new(),
            model_prefix: String::new(),
            api_key: None,
            provider_headers: "{}".to_string(),
            supported_endpoints: row.supported_endpoints.clone(),
            cost: row.cost.clone(),
            cost_input_override: row.cost_input_override,
            cost_output_override: row.cost_output_override,
            cost_cache_read_override: row.cost_cache_read_override,
            cost_cache_write_override: row.cost_cache_write_override,
            context_limit: None,
            input_limit: None,
            output_limit: None,
            provider_enabled: None,
            model_enabled: None,
            tool_search_supported: 1,
            timeout_seconds: None,
            cooldown_seconds: None,
            max_concurrency: None,
            queue_timeout_seconds: None,
            model_max_concurrency: None,
            model_queue_timeout_seconds: None,
            provider_health: row.provider_health,
            upstream_model: row.upstream_model.clone(),
            weight: row.target_weight,
            priority: row.target_priority,
            enabled: 1,
            provider_api_key_id: None,
            provider_api_key_name: None,
            auth_retryable: false,
        })
        .collect::<Vec<_>>();
    let metrics = runtime_target_metrics(state, &targets).await?;
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let ordered = order_targets(
        state,
        route_id.unwrap_or(-1),
        strategy.as_str(),
        targets,
        Some(session_id),
    )
    .await?;
    Ok(Some(
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                let metric = metrics.get(&(target.provider_id, target.upstream_model.clone()));
                let (input_cost_per_million, output_cost_per_million) = target_cost_prices(&target);
                let decision_reason =
                    target_decision_reason(strategy, &target, metric, index == 0).to_string();
                RouteDiagnoseRuntimeTarget {
                    order: index + 1,
                    provider_id: target.provider_id,
                    provider_name: target.provider_name,
                    upstream_model: target.upstream_model,
                    input_cost_per_million,
                    output_cost_per_million,
                    avg_latency_ms: metric.and_then(|metric| metric.avg_latency_ms),
                    recent_requests: metric.map(|metric| metric.requests),
                    decision_reason,
                    provider_api_key_id: target.provider_api_key_id,
                    provider_api_key_name: target.provider_api_key_name,
                    provider_health: target.provider_health.map(|value| value != 0),
                }
            })
            .collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve_route_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
) -> AppResult<ResolvedRoute> {
    let model_patterns = api_key_model_patterns(api_key)?;
    match resolve_route_with_patterns(state, model, endpoint, model_patterns.as_deref()).await {
        Ok(route) => Ok(route),
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

pub(super) async fn find_explicit_route(state: &AppState, model: &str) -> AppResult<Option<Route>> {
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, model_pattern,
               CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy,
               enabled, created_at, updated_at
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

pub(super) async fn find_prefixed_targets(
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

#[derive(Debug, sqlx::FromRow)]
struct AutoTargetRow {
    public_id: String,
    #[sqlx(flatten)]
    target: RouteTarget,
}

/// Loads every enabled provider model that can serve the requested endpoint.
///
/// Auto is intentionally catalog-wide rather than backed by a route row. API
/// key model permissions still apply to the concrete target IDs and upstream
/// model names, so `auto` cannot be used to escape a restricted key.
pub(super) async fn find_auto_targets(
    state: &AppState,
    endpoint: &str,
    model_patterns: Option<&[String]>,
) -> AppResult<Vec<RouteTarget>> {
    let rows = sqlx::query_as::<_, AutoTargetRow>(
        r#"
        SELECT p.model_prefix || pm.model_name AS public_id,
               NULL AS id, NULL AS route_id, p.id AS provider_id,
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
               COALESCE(pm.output_override, pm.output_limit) AS output_limit,
               p.enabled AS provider_enabled,
               pm.enabled AS model_enabled,
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
        ORDER BY p.id, pm.model_name
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let all_targets = rows
        .iter()
        .map(|row| row.target.clone())
        .collect::<Vec<_>>();
    let all_targets = filter_targets_for_endpoint(all_targets, endpoint);
    if all_targets.is_empty() {
        return Err(AppError::NotFound(format!(
            "no enabled provider model can serve endpoint '{endpoint}'"
        )));
    }
    if model_patterns.is_none() {
        return Ok(all_targets);
    }

    let targets = rows
        .into_iter()
        .filter(|row| {
            model_matches_patterns(model_patterns, &row.public_id)
                || model_matches_patterns(model_patterns, &row.target.upstream_model)
        })
        .map(|row| row.target)
        .collect::<Vec<_>>();
    let targets = filter_targets_for_endpoint(targets, endpoint);
    if targets.is_empty() {
        return Err(AppError::Forbidden(
            "API key model permissions do not allow any auto target".to_string(),
        ));
    }
    Ok(targets)
}

pub(super) async fn load_targets(state: &AppState, route_id: i64) -> AppResult<Vec<RouteTarget>> {
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

pub(super) fn provider_health_rank(health: Option<i64>) -> u8 {
    match health {
        Some(0) => 2,
        Some(_) => 0,
        None => 1,
    }
}

pub(super) const PROVIDER_KEY_TOUCH_INTERVAL: Duration = Duration::from_secs(60);

pub(super) fn bounded_retry_after(duration: Duration, max: Duration) -> Option<Duration> {
    (!duration.is_zero()).then(|| duration.min(max))
}

/// Exponential backoff for a same-target retry, capped by the operator setting.
pub(super) fn retry_backoff(
    settings: &crate::models::ResilienceSettings,
    retry_index: u32,
) -> Duration {
    retry_backoff_with_draw(settings, retry_index, rand::random::<f64>())
}

/// Deterministic form of [`retry_backoff`] used by tests.
///
/// The jitter only extends the delay, never shortens it, so an operator's
/// configured minimum remains a real floor. The configured cap still wins.
pub(super) fn retry_backoff_with_draw(
    settings: &crate::models::ResilienceSettings,
    retry_index: u32,
    draw: f64,
) -> Duration {
    let base = settings.retry_backoff_ms.max(0) as u64;
    let cap = settings.retry_max_backoff_ms.max(0) as u64;
    let multiplier = 1u64 << retry_index.min(6);
    let delay = base.saturating_mul(multiplier).min(cap);
    let jitter_percent = (draw.clamp(0.0, 1.0) * 25.0).round() as u64;
    let jittered = delay
        .saturating_mul(100u64.saturating_add(jitter_percent))
        / 100;
    Duration::from_millis(jittered.min(cap))
}

pub(super) fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return bounded_retry_after(Duration::from_millis(value), MAX_UPSTREAM_RETRY_AFTER);
    }

    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return bounded_retry_after(Duration::from_secs(seconds), MAX_UPSTREAM_RETRY_AFTER);
    }

    let deadline = httpdate::parse_http_date(value).ok()?;
    let duration = deadline.duration_since(std::time::SystemTime::now()).ok()?;
    bounded_retry_after(duration, MAX_UPSTREAM_RETRY_AFTER)
}

pub(super) fn provider_cooldown(
    status: Option<StatusCode>,
    failures: u32,
    retry_after: Option<Duration>,
    configured_seconds: Option<i64>,
) -> Duration {
    let default_base = match status {
        Some(StatusCode::TOO_MANY_REQUESTS) => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    let configured_base = configured_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    let base = match configured_base {
        Some(configured) => configured.max(retry_after.unwrap_or_default()),
        None => retry_after.unwrap_or(default_base),
    };
    let exponent = failures.saturating_sub(1).min(4);
    let cap = if configured_base.is_some() {
        MAX_CONFIGURED_PROVIDER_COOLDOWN
    } else {
        MAX_PROVIDER_COOLDOWN
    };
    base.saturating_mul(1u32 << exponent).min(cap)
}

pub(super) fn provider_key_cooldown(status: StatusCode, retry_after: Option<Duration>) -> Duration {
    let base = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Duration::from_secs(300),
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    retry_after
        .map(|duration| base.max(duration))
        .unwrap_or(base)
        .min(MAX_PROVIDER_KEY_COOLDOWN)
}

pub(super) fn target_cooldown(
    status: StatusCode,
    retry_after: Option<Duration>,
    configured_seconds: Option<i64>,
) -> Duration {
    let default_base = match status {
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    let configured_base = configured_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    let base = match configured_base {
        Some(configured) => configured.max(retry_after.unwrap_or_default()),
        None => retry_after.unwrap_or(default_base),
    };
    let cap = if configured_base.is_some() {
        MAX_CONFIGURED_PROVIDER_COOLDOWN
    } else {
        MAX_TARGET_COOLDOWN
    };
    base.min(cap)
}

pub(super) async fn mark_provider_error(
    state: &AppState,
    provider_id: i64,
    status: Option<StatusCode>,
    retry_after: Option<Duration>,
    configured_cooldown_seconds: Option<i64>,
) {
    let failures = {
        let mut streaks = state.provider_failure_streak.lock().await;
        let failures = streaks.entry(provider_id).or_default();
        *failures = failures.saturating_add(1);
        *failures
    };
    state.provider_cooldown.lock().await.insert(
        provider_id,
        Instant::now()
            + provider_cooldown(status, failures, retry_after, configured_cooldown_seconds),
    );
}

pub(super) async fn mark_target_error(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    retry_after: Option<Duration>,
) -> usize {
    let now = Instant::now();
    let mut cooldowns = state.target_cooldown.lock().await;
    cooldowns.retain(|_, until| *until > now);
    cooldowns.insert(
        (target.provider_id, target.upstream_model.clone()),
        now + target_cooldown(status, retry_after, target.cooldown_seconds),
    );
    cooldowns
        .keys()
        .filter(|(provider_id, _)| *provider_id == target.provider_id)
        .count()
}

pub(super) async fn mark_target_success(state: &AppState, provider_id: i64, upstream_model: &str) {
    state
        .target_cooldown
        .lock()
        .await
        .remove(&(provider_id, upstream_model.to_string()));
}

/// Cools a target after its response stream failed mid-flight.
///
/// A stream that dies after the headers were sent (idle timeout, reset) is the
/// same class of transport fault as a pre-header failure, so it takes the
/// provider-wide path `send_provider_request` uses. The client already has a
/// truncated answer, but without this the next request would pick the same
/// unhealthy provider and stall again.
pub(super) async fn mark_stream_failure(state: &AppState, target: &RouteTarget, message: &str) {
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        %message,
        "upstream stream failed before it finished; cooling the provider"
    );
    mark_provider_error(state, target.provider_id, None, None, target.cooldown_seconds).await;
    mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
}

pub(super) async fn record_upstream_failure(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    response_headers: &HeaderMap,
    message: &str,
) {
    let retry_after = retry_after_from_headers(response_headers);
    if retryable_status(status) {
        let active_model_cooldowns = mark_target_error(state, target, status, retry_after).await;
        // A model-level error must not remove every healthy sibling model on
        // the provider. Only escalate to provider-wide isolation after enough
        // distinct models have failed; transport errors take the direct path
        // in `send_provider_request` because they are provider-wide by nature.
        if provider_circuit_should_open(status, active_model_cooldowns) {
            mark_provider_error(
                state,
                target.provider_id,
                Some(status),
                retry_after,
                target.cooldown_seconds,
            )
            .await;
        }
    }
    if provider_key_failure(status) {
        mark_provider_api_key_error(
            state,
            target.provider_api_key_id,
            status,
            &format!("{} returned {}: {}", target.provider_name, status, message),
            retry_after,
        )
        .await;
    }
}

pub(super) fn provider_circuit_should_open(
    status: StatusCode,
    active_model_cooldowns: usize,
) -> bool {
    // A 409 usually describes this request's resource state, not the
    // provider's health, so it should fall through without opening a circuit.
    status != StatusCode::CONFLICT
        && active_model_cooldowns >= PROVIDER_MODEL_COOLDOWN_THRESHOLD
}

pub(super) async fn mark_provider_api_key_used(state: &AppState, provider_api_key_id: Option<i64>) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    {
        let mut touched = state.provider_key_touched.lock().await;
        if touched
            .get(&provider_api_key_id)
            .is_some_and(|last| last.elapsed() < PROVIDER_KEY_TOUCH_INTERVAL)
        {
            return;
        }
        touched.insert(provider_api_key_id, Instant::now());
    }

    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to update provider key usage");
        }
    });
}

pub(super) async fn mark_provider_success(state: &AppState, provider_id: i64) {
    state.provider_cooldown.lock().await.remove(&provider_id);
    state
        .provider_failure_streak
        .lock()
        .await
        .remove(&provider_id);
}

/// The outcome of an upstream attempt that may have been resent once.
pub(super) enum UpstreamAttempt {
    /// The upstream answered successfully; the body has not been consumed.
    Ok(UpstreamResponse),
    /// The upstream failed; the status, headers and body are already read.
    Error {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
}

/// Reads the retry policy for the data path.
///
/// A broken or missing settings row must never take inference down, so a read
/// failure degrades to the built-in defaults with a warning instead of
/// propagating.
async fn resilience_policy(state: &AppState) -> crate::models::ResilienceSettings {
    match state.resilience_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "failed to load resilience settings; using defaults");
            crate::models::ResilienceSettings::default()
        }
    }
}

/// The result of one upstream send attempt, before retry decisions.
enum SendError {
    /// The transport itself failed (connect, reset, timeout, body).
    Http(reqwest::Error),
    /// A streaming call did not receive response headers within its budget.
    HeaderTimeout(Duration),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "{error}"),
            Self::HeaderTimeout(budget) => {
                write!(formatter, "no response headers within {}s", budget.as_secs())
            }
        }
    }
}

pub(super) async fn send_provider_request(
    state: &AppState,
    target: &RouteTarget,
    streamed: bool,
    build: impl Fn() -> AppResult<RequestBuilder>,
) -> AppResult<reqwest::Response> {
    let resilience = resilience_policy(state).await;
    let mut retries_left = resilience.max_retries.clamp(0, MAX_SAME_TARGET_RETRIES) as u32;
    let mut retries_used = 0u32;

    loop {
        let request = build()?;
        let budget = target
            .timeout_seconds
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
        // A total-request timeout would abort a long streaming generation, so a
        // streamed call only gets its provider budget on the wait for response
        // headers; the shared client's idle read timeout governs the body. A
        // buffered call keeps the total timeout because it has no stream to
        // protect and an unbounded body read would be a memory risk.
        let outcome = match budget {
            Some(budget) if streamed => match tokio::time::timeout(budget, request.send()).await {
                Ok(result) => result.map_err(SendError::Http),
                Err(_) => Err(SendError::HeaderTimeout(budget)),
            },
            Some(budget) => request.timeout(budget).send().await.map_err(SendError::Http),
            None => request.send().await.map_err(SendError::Http),
        };

        match outcome {
            Ok(response) => return Ok(response),
            Err(error) => {
                let retryable = match &error {
                    SendError::Http(error) => retryable_transport_error(error),
                    SendError::HeaderTimeout(_) => true,
                };
                // Transport faults are worth one more attempt: a reset socket or
                // a slow upstream often recovers without the caller noticing.
                if retries_left > 0 && retryable {
                    retries_left -= 1;
                    let delay = retry_backoff(&resilience, retries_used);
                    retries_used += 1;
                    tracing::warn!(
                        provider = %target.provider_name,
                        model = %target.upstream_model,
                        retry = retries_used,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "upstream transport error; retrying the same target"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                mark_provider_error(
                    state,
                    target.provider_id,
                    None,
                    None,
                    target.cooldown_seconds,
                )
                .await;
                mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
                return Err(AppError::Upstream(format!(
                    "{} request failed: {error}",
                    target.provider_name
                )));
            }
        }
    }
}

async fn acquire_limit_slot(
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    limit: usize,
    wait: Duration,
    label: String,
    gate_closed_message: &'static str,
) -> AppResult<OwnedSemaphorePermit> {
    if wait.is_zero() {
        return semaphore
            .try_acquire_owned()
            .map_err(|_| AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!("{label} is at its concurrency limit ({limit}); retry shortly"),
                retry_after: Some(Duration::from_secs(1)),
            });
    }

    match tokio::time::timeout(wait, semaphore.acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(AppError::Upstream(gate_closed_message.to_string())),
        Err(_) => Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "{label} stayed at its concurrency limit ({limit}) for {} seconds",
                wait.as_secs()
            ),
            retry_after: Some(Duration::from_secs(1)),
        }),
    }
}

/// Waits for model and provider concurrency slots. Model slots are acquired
/// first so a queue of requests for one hot model cannot occupy every provider
/// slot and starve healthy sibling models.
pub(super) async fn acquire_upstream_slots(
    state: &AppState,
    target: &RouteTarget,
) -> Result<Option<UpstreamSlots>, AcquireSlotError> {
    let model = match target.model_max_concurrency.filter(|limit| *limit > 0) {
        Some(limit) => {
            let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
            let semaphore = {
                let mut semaphores = state.model_concurrency.lock().await;
                semaphores
                    .entry((target.provider_id, target.upstream_model.clone()))
                    .or_insert_with(|| {
                        std::sync::Arc::new(tokio::sync::Semaphore::new(limit_usize.max(1)))
                    })
                    .clone()
            };
            let wait = target
                .model_queue_timeout_seconds
                .map(|seconds| Duration::from_secs(seconds.unsigned_abs()))
                .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
            Some(
                acquire_limit_slot(
                    semaphore,
                    limit_usize,
                    wait,
                    format!(
                        "{}/{}",
                        target.provider_name, target.upstream_model
                    ),
                    "model concurrency gate closed",
                )
                .await
                .map_err(AcquireSlotError::Model)?,
            )
        }
        None => None,
    };

    let provider = match target.max_concurrency.filter(|limit| *limit > 0) {
        Some(limit) => {
            let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
            let semaphore = {
                let mut semaphores = state.provider_concurrency.lock().await;
                semaphores
                    .entry(target.provider_id)
                    .or_insert_with(|| {
                        std::sync::Arc::new(tokio::sync::Semaphore::new(limit_usize.max(1)))
                    })
                    .clone()
            };
            let wait = target
                .queue_timeout_seconds
                .map(|seconds| Duration::from_secs(seconds.unsigned_abs()))
                .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
            Some(
                acquire_limit_slot(
                    semaphore,
                    limit_usize,
                    wait,
                    target.provider_name.clone(),
                    "provider concurrency gate closed",
                )
                .await
                .map_err(AcquireSlotError::Provider)?,
            )
        }
        None => None,
    };

    if model.is_none() && provider.is_none() {
        Ok(None)
    } else {
        Ok(Some(UpstreamSlots { provider, model }))
    }
}

/// Sends a request with the compatibility retries shared by every forwarding
/// path:
///
/// - resend once when the upstream answers with the opaque aggregator 4xx (an
///   `invalid request error` carrying only a trace id) that usually means an
///   internal channel failed rather than a bad request;
/// - if an upstream still rejects `tool_search`, strip it, remember that the
///   provider does not support it, and retry the same target once without it.
///
/// `body` is the exact body sent upstream, so compatibility retries update the
/// request rather than replaying the rejected payload.
pub(super) async fn send_provider_request_with_compat_retry<F>(
    state: &AppState,
    target: &RouteTarget,
    request_id: &str,
    endpoint: &str,
    streamed: bool,
    body: &mut Value,
    build: F,
) -> AppResult<UpstreamAttempt>
where
    F: Fn(&Value) -> AppResult<RequestBuilder>,
{
    let resilience = resilience_policy(state).await;
    let mut retries_left = resilience.max_retries.clamp(0, MAX_SAME_TARGET_RETRIES) as u32;
    let mut retries_used = 0u32;
    let mut stream_retries_left = if streamed && resilience.stream_recovery_enabled {
        resilience
            .stream_recovery_max_retries
            .clamp(0, crate::models::STREAM_RECOVERY_MAX_RETRIES) as u32
    } else {
        0
    };
    let mut stream_retries_used = 0u32;

    let mut response = UpstreamResponse::new(
        send_provider_request(state, target, streamed, || build(body)).await?,
    );
    let mut transient_retry_used = false;

    loop {
        let status = response.status();
        if status.is_success() {
            if streamed && resilience.stream_recovery_enabled {
                match response.recover_prefix().await {
                    Ok(recovered) => return Ok(UpstreamAttempt::Ok(recovered)),
                    Err(error) if stream_retries_left > 0 => {
                        stream_retries_left -= 1;
                        let delay = retry_backoff(&resilience, stream_retries_used);
                        stream_retries_used += 1;
                        tracing::warn!(
                            provider = %target.provider_name,
                            model = %target.upstream_model,
                            request_id,
                            endpoint,
                            retry = stream_retries_used,
                            delay_ms = delay.as_millis() as u64,
                            %error,
                            "upstream stream ended before commit; retrying the same target"
                        );
                        tokio::time::sleep(delay).await;
                        response = UpstreamResponse::new(
                            send_provider_request(state, target, streamed, || build(body)).await?,
                        );
                        continue;
                    }
                    Err(error) => {
                        mark_provider_error(
                            state,
                            target.provider_id,
                            Some(StatusCode::BAD_GATEWAY),
                            None,
                            target.cooldown_seconds,
                        )
                        .await;
                        mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
                        return Err(AppError::Upstream(format!(
                            "{} stream ended before commit: {error}",
                            target.provider_name
                        )));
                    }
                }
            }
            return Ok(UpstreamAttempt::Ok(response));
        }
        let headers = response.headers().clone();
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;

        // One bounded retry against the same target turns a momentary 5xx into
        // a served request instead of a fallback or a client-visible failure.
        if retries_left > 0 && retryable_same_target_status(status) {
            retries_left -= 1;
            let delay = retry_backoff(&resilience, retries_used);
            retries_used += 1;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                retry = retries_used,
                delay_ms = delay.as_millis() as u64,
                "upstream returned a transient status; retrying the same target"
            );
            tokio::time::sleep(delay).await;
            response = UpstreamResponse::new(
                send_provider_request(state, target, streamed, || build(body)).await?,
            );
            continue;
        }

        if !transient_retry_used && transient_upstream_4xx(status, &response_body) {
            transient_retry_used = true;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                trace_id = tracing::field::display(
                    upstream_trace_id(&response_body).as_deref().unwrap_or("unknown")
                ),
                has_tool_search = strip_tool_search_tools(body).is_some(),
                "upstream returned an opaque 4xx; retrying once"
            );
            response = UpstreamResponse::new(
                send_provider_request(state, target, streamed, || build(body)).await?,
            );
            continue;
        }

        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) && upstream_rejects_tool_search(&response_body)
            && let Some(compat_body) = strip_tool_search_tools(body)
        {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                "upstream rejected tool_search; retrying without it"
            );
            mark_provider_tool_search_unsupported(state, target.provider_id).await;
            *body = compat_body;
            response = UpstreamResponse::new(
                send_provider_request(state, target, streamed, || build(body)).await?,
            );
            continue;
        }

        if transient_retry_used && transient_upstream_4xx(status, &response_body) {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                %status,
                trace_id = tracing::field::display(
                    upstream_trace_id(&response_body).as_deref().unwrap_or("unknown")
                ),
                "upstream still returned an opaque 4xx after retry"
            );
        }

        return Ok(UpstreamAttempt::Error {
            status,
            headers,
            body: response_body,
        });
    }
}

pub(super) async fn mark_provider_api_key_error(
    state: &AppState,
    provider_api_key_id: Option<i64>,
    status: StatusCode,
    message: &str,
    retry_after: Option<Duration>,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    state.provider_key_cooldown.lock().await.insert(
        provider_api_key_id,
        Instant::now() + provider_key_cooldown(status, retry_after),
    );
    state
        .provider_key_error_state
        .lock()
        .await
        .insert(provider_api_key_id);
    let message = message.chars().take(1000).collect::<String>();
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error = ? \
             WHERE id = ?",
        )
        .bind(message)
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to record provider key error");
        }
    });
}

pub(super) async fn mark_provider_api_key_success(
    state: &AppState,
    provider_api_key_id: Option<i64>,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    state
        .provider_key_cooldown
        .lock()
        .await
        .remove(&provider_api_key_id);
    let had_error = state
        .provider_key_error_state
        .lock()
        .await
        .remove(&provider_api_key_id);
    if !had_error {
        return;
    }
    if let Err(error) = sqlx::query(
        "UPDATE provider_api_keys \
         SET last_error_at = NULL, last_error = NULL \
         WHERE id = ?",
    )
    .bind(provider_api_key_id)
    .execute(&state.pool)
    .await
    {
        state
            .provider_key_error_state
            .lock()
            .await
            .insert(provider_api_key_id);
        tracing::warn!(
            %error,
            provider_api_key_id,
            "failed to clear recovered provider key error"
        );
    }
}

pub(super) fn stable_hash64(parts: &[&[u8]]) -> u64 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 digests contain at least eight bytes"),
    )
}

pub(super) fn session_target_hash(
    session_id: &str,
    ordering_key: i64,
    target: &RouteTarget,
) -> u64 {
    stable_hash64(&[
        b"openllm-session-target-v1",
        &ordering_key.to_be_bytes(),
        &target.id.to_be_bytes(),
        &target.provider_id.to_be_bytes(),
        target.upstream_model.as_bytes(),
        session_id.as_bytes(),
    ])
}

pub(super) fn session_provider_key_hash(session_id: &str, provider_id: i64, key_id: i64) -> u64 {
    stable_hash64(&[
        b"openllm-session-provider-key-v1",
        &provider_id.to_be_bytes(),
        &key_id.to_be_bytes(),
        session_id.as_bytes(),
    ])
}

/// Orders weighted and round-robin targets without storing per-session state.
///
/// Weighted routes use weighted rendezvous hashing, so sessions still spread
/// according to target weights while a given session keeps the same preference
/// order. Priority routes already have a deterministic order and are left
/// untouched.
pub(super) fn order_targets_for_session(
    session_id: &str,
    ordering_key: i64,
    strategy: RouteStrategy,
    targets: &mut Vec<RouteTarget>,
) {
    match strategy {
        RouteStrategy::Priority => {}
        RouteStrategy::Weighted => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    let hash = session_target_hash(session_id, ordering_key, &target);
                    let uniform = (hash as f64 + 1.0) / (u64::MAX as f64 + 1.0);
                    let score = -uniform.ln() / target.weight.max(1) as f64;
                    (score, target)
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .total_cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
        RouteStrategy::RoundRobin => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    (
                        session_target_hash(session_id, ordering_key, &target),
                        target,
                    )
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
        RouteStrategy::CostOptimized
        | RouteStrategy::LatencyOptimized
        | RouteStrategy::LeastUsed => {}
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RuntimeTargetMetricRow {
    provider_id: i64,
    upstream_model: String,
    requests: i64,
    avg_latency_ms: Option<f64>,
}

async fn runtime_target_metrics(
    state: &AppState,
    targets: &[RouteTarget],
) -> AppResult<HashMap<(i64, String), RuntimeTargetMetricRow>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let provider_ids = targets
        .iter()
        .map(|target| target.provider_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        SELECT provider_id,
               upstream_model,
               COUNT(*) AS requests,
               AVG(
                   CASE
                       WHEN success = 1
                            AND first_token_ms IS NOT NULL
                            AND first_token_ms > 0
                           THEN first_token_ms
                       WHEN success = 1 AND latency_ms > 0
                           THEN latency_ms
                       ELSE NULL
                   END
               ) AS avg_latency_ms
        FROM usage_logs
        WHERE in_flight = 0
          AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')
          AND provider_id IN (
        "#,
    );
    {
        let mut separated = query.separated(", ");
        for provider_id in provider_ids {
            separated.push_bind(provider_id);
        }
    }
    query.push(
        r#")
        GROUP BY provider_id, upstream_model
        "#,
    );
    let rows = query
        .build_query_as::<RuntimeTargetMetricRow>()
        .fetch_all(&state.pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| ((row.provider_id, row.upstream_model.clone()), row))
        .collect())
}

fn target_cost_prices(target: &RouteTarget) -> (Option<f64>, Option<f64>) {
    let cost = target
        .cost
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let Some(cost) = effective_cost_value(
        cost.as_ref(),
        target.cost_input_override,
        target.cost_output_override,
        target.cost_cache_read_override,
        target.cost_cache_write_override,
    ) else {
        return (None, None);
    };
    (
        crate::models::cost_base_price(&cost, "input"),
        crate::models::cost_base_price(&cost, "output"),
    )
}

fn target_cost_score(target: &RouteTarget) -> Option<f64> {
    let (input, output) = target_cost_prices(target);
    match (input, output) {
        (Some(input), Some(output)) => Some(input * 0.7 + output * 0.3),
        (Some(input), None) => Some(input),
        (None, Some(output)) => Some(output),
        (None, None) => None,
    }
}

/// Hard per-request cost ceiling supplied by the caller.
pub(super) const BUDGET_USD_HEADER: &str = "x-openllm-budget-usd";

/// Per-request routing controls. Each is optional; when absent the route's own
/// configuration decides.
pub(super) const STRATEGY_HEADER: &str = "x-openllm-strategy";
pub(super) const PROVIDER_HEADER: &str = "x-openllm-provider";
pub(super) const EXCLUDE_PROVIDERS_HEADER: &str = "x-openllm-exclude-providers";

/// The routing controls a caller supplied for one request.
#[derive(Debug, Default, Clone)]
pub(super) struct RequestRoutingOverrides {
    /// Ordered strategy name that replaced the route's configured strategy.
    pub strategy: Option<String>,
    /// Provider selector the caller pinned the request to, as supplied.
    pub provider: Option<String>,
    /// Providers the caller excluded, as supplied.
    pub excluded_providers: Vec<String>,
}

impl RequestRoutingOverrides {
    pub fn is_empty(&self) -> bool {
        self.strategy.is_none() && self.provider.is_none() && self.excluded_providers.is_empty()
    }
}

/// Builds request routing overrides from an API key's stored policy.
///
/// A malformed or absent policy yields no overrides so a bad row can never
/// block traffic; write-time validation is what keeps stored values clean.
pub(super) fn routing_overrides_from_key(api_key: Option<&ApiKeyRecord>) -> RequestRoutingOverrides {
    let policy = crate::models::parse_routing_policy(
        api_key.and_then(|api_key| api_key.routing_policy.as_deref()),
    );
    RequestRoutingOverrides {
        strategy: policy.strategy,
        provider: policy.provider,
        excluded_providers: policy.exclude_providers,
    }
}

/// Layers per-request overrides on top of a key's policy. Each field is
/// independent: a request may override just the strategy and keep the key's
/// provider pin.
pub(super) fn merge_routing_overrides(
    policy: RequestRoutingOverrides,
    request: RequestRoutingOverrides,
) -> RequestRoutingOverrides {
    RequestRoutingOverrides {
        strategy: request.strategy.or(policy.strategy),
        provider: request.provider.or(policy.provider),
        excluded_providers: if request.excluded_providers.is_empty() {
            policy.excluded_providers
        } else {
            request.excluded_providers
        },
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> AppResult<Option<String>> {
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::BadRequest(format!("{name} must be valid ASCII")))?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

/// Parses the optional per-request routing headers.
///
/// A malformed value is rejected rather than ignored: silently dropping a
/// caller's routing instruction could send traffic to a provider they meant to
/// avoid.
pub(super) fn request_routing_overrides(headers: &HeaderMap) -> AppResult<RequestRoutingOverrides> {
    let strategy = header_text(headers, STRATEGY_HEADER)?
        .map(|value| {
            RouteStrategy::from_str(&value)
                .map(|strategy| strategy.as_str().to_string())
                .map_err(AppError::BadRequest)
        })
        .transpose()?;
    let provider = header_text(headers, PROVIDER_HEADER)?;
    let excluded_providers = header_text(headers, EXCLUDE_PROVIDERS_HEADER)?
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(RequestRoutingOverrides {
        strategy,
        provider,
        excluded_providers,
    })
}

/// Whether a selector names this target's provider, by numeric id or by name
/// (case-insensitive).
fn selector_matches(selector: &str, target: &RouteTarget) -> bool {
    if let Ok(id) = selector.parse::<i64>() {
        return id == target.provider_id;
    }
    selector.eq_ignore_ascii_case(target.provider_name.trim())
}

/// Applies per-request routing controls to a resolved route.
///
/// Returns how many targets the filters removed. Pinning and excluding can
/// leave nothing behind; that is a client error, not a routing fallback.
pub(super) fn apply_routing_overrides(
    resolved: &mut ResolvedRoute,
    model: &str,
    overrides: &RequestRoutingOverrides,
) -> AppResult<usize> {
    if overrides.is_empty() {
        return Ok(0);
    }
    if let Some(strategy) = &overrides.strategy {
        resolved.strategy = strategy.clone();
    }
    if let Some(provider) = &overrides.provider {
        resolved
            .targets
            .retain(|target| selector_matches(provider, target));
        if resolved.targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "{PROVIDER_HEADER} '{provider}' does not match any target of model '{}'",
                model
            )));
        }
    }
    if !overrides.excluded_providers.is_empty() {
        let before = resolved.targets.len();
        resolved.targets.retain(|target| {
            !overrides
                .excluded_providers
                .iter()
                .any(|selector| selector_matches(selector, target))
        });
        if resolved.targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "{EXCLUDE_PROVIDERS_HEADER} removed every target of model '{}'",
                model
            )));
        }
        return Ok(before - resolved.targets.len());
    }
    Ok(0)
}

/// Echoes the routing controls that took effect so a caller can attribute the
/// response to the provider they asked for.
pub(super) fn apply_override_headers(response: &mut Response, overrides: &RequestRoutingOverrides) {
    if overrides.is_empty() {
        return;
    }
    let mut insert = |name: &'static str, value: String| {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    };
    if let Some(strategy) = &overrides.strategy {
        insert(STRATEGY_HEADER, strategy.clone());
    }
    if let Some(provider) = &overrides.provider {
        insert(PROVIDER_HEADER, provider.clone());
    }
    if !overrides.excluded_providers.is_empty() {
        insert(
            EXCLUDE_PROVIDERS_HEADER,
            overrides.excluded_providers.join(","),
        );
    }
}

/// Parses the optional per-request budget header.
///
/// The value is a USD ceiling for one request. A malformed or non-positive
/// value is rejected rather than ignored: silently dropping it would let a
/// caller believe a cost guarantee was in force when it was not.
pub(super) fn budget_usd_from_headers(headers: &HeaderMap) -> AppResult<Option<f64>> {
    let Some(raw) = headers.get(BUDGET_USD_HEADER) else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .map_err(|_| {
            AppError::BadRequest(format!("{BUDGET_USD_HEADER} must be a positive number"))
        })?
        .trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let budget = raw.parse::<f64>().map_err(|_| {
        AppError::BadRequest(format!(
            "{BUDGET_USD_HEADER} must be a positive number, got '{raw}'"
        ))
    })?;
    if !budget.is_finite() || budget <= 0.0 {
        return Err(AppError::BadRequest(format!(
            "{BUDGET_USD_HEADER} must be a positive number, got '{raw}'"
        )));
    }
    Ok(Some(budget))
}

/// Estimates what one request would cost on a target, in USD.
///
/// Returns `None` when the target has no usable price, because an unknown cost
/// cannot be proven to fit under a hard ceiling.
pub(super) fn estimate_target_cost_usd(
    target: &RouteTarget,
    prompt_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    let (input_price, output_price) = target_cost_prices(target);
    if input_price.is_none() && output_price.is_none() {
        return None;
    }
    let prompt_tokens = prompt_tokens.max(0) as f64;
    let output_tokens = output_tokens.max(0) as f64;
    // Cache reads are billed at or below the fresh-input price, so charging
    // every prompt token at the input rate is the conservative estimate.
    let cost = prompt_tokens * input_price.unwrap_or(0.0) / 1_000_000.0
        + output_tokens * output_price.unwrap_or(0.0) / 1_000_000.0;
    Some(cost)
}

/// Drops every target whose estimated cost exceeds `budget_usd`.
///
/// Returns how many targets were removed. An error is returned when nothing
/// survives, including when the surviving set is empty only because no target
/// publishes pricing.
pub(super) fn apply_budget_filter(
    targets: &mut Vec<RouteTarget>,
    budget_usd: f64,
    prompt_tokens: i64,
    output_tokens: i64,
) -> AppResult<usize> {
    let before = targets.len();
    let mut cheapest: Option<f64> = None;
    let mut unpriced = 0usize;
    targets.retain(
        |target| match estimate_target_cost_usd(target, prompt_tokens, output_tokens) {
            Some(cost) => {
                cheapest = Some(cheapest.map_or(cost, |current| current.min(cost)));
                cost <= budget_usd
            }
            None => {
                unpriced += 1;
                false
            }
        },
    );
    let removed = before - targets.len();
    if !targets.is_empty() {
        return Ok(removed);
    }
    let detail = match cheapest {
        Some(cost) => format!("the cheapest known estimate is ${cost:.6}"),
        None if unpriced > 0 => {
            "no candidate publishes pricing, so the ceiling cannot be verified".to_string()
        }
        None => "no candidate is available".to_string(),
    };
    Err(AppError::BadRequest(format!(
        "no route target fits the ${budget_usd:.6} budget for this request: {detail}"
    )))
}

fn target_decision_reason(
    strategy: RouteStrategy,
    target: &RouteTarget,
    metric: Option<&RuntimeTargetMetricRow>,
    first: bool,
) -> &'static str {
    if target.provider_health == Some(0) {
        return "provider_health_failed";
    }
    match strategy {
        RouteStrategy::CostOptimized => {
            let (input, output) = target_cost_prices(target);
            if input.is_none() && output.is_none() {
                "unknown_cost"
            } else if first {
                "lowest_known_cost"
            } else {
                "known_cost"
            }
        }
        RouteStrategy::LatencyOptimized => match metric.and_then(|metric| metric.avg_latency_ms) {
            Some(_) if first => "lowest_recent_latency",
            Some(_) => "recent_latency",
            None => "no_recent_latency",
        },
        RouteStrategy::LeastUsed => match metric {
            Some(_) if first => "least_recent_traffic",
            Some(_) => "recent_traffic",
            None => "no_recent_traffic",
        },
        RouteStrategy::Priority => "priority_order",
        RouteStrategy::Weighted | RouteStrategy::RoundRobin => "session_affinity",
    }
}

fn sort_targets_by_strategy_metrics(
    strategy: RouteStrategy,
    targets: &mut [RouteTarget],
    metrics: &HashMap<(i64, String), RuntimeTargetMetricRow>,
) {
    let metric_for =
        |target: &RouteTarget| metrics.get(&(target.provider_id, target.upstream_model.clone()));
    match strategy {
        RouteStrategy::CostOptimized => {
            targets.sort_by(|left, right| {
                target_cost_score(left)
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&target_cost_score(right).unwrap_or(f64::INFINITY))
                    .then_with(|| left.priority.cmp(&right.priority))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        RouteStrategy::LatencyOptimized => {
            targets.sort_by(|left, right| {
                let left_latency = metric_for(left)
                    .and_then(|metric| metric.avg_latency_ms)
                    .unwrap_or(f64::INFINITY);
                let right_latency = metric_for(right)
                    .and_then(|metric| metric.avg_latency_ms)
                    .unwrap_or(f64::INFINITY);
                left_latency
                    .total_cmp(&right_latency)
                    .then_with(|| left.priority.cmp(&right.priority))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        RouteStrategy::LeastUsed => {
            targets.sort_by(|left, right| {
                let left_requests = metric_for(left).map(|metric| metric.requests).unwrap_or(0);
                let right_requests = metric_for(right).map(|metric| metric.requests).unwrap_or(0);
                left_requests
                    .cmp(&right_requests)
                    .then_with(|| left.priority.cmp(&right.priority))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        RouteStrategy::Priority | RouteStrategy::Weighted | RouteStrategy::RoundRobin => {}
    }
}

pub(super) async fn expand_target_provider_keys(
    state: &AppState,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
) -> AppResult<Vec<RouteTarget>> {
    let mut keys_by_provider = HashMap::new();
    for target in &targets {
        if keys_by_provider.contains_key(&target.provider_id) {
            continue;
        }
        let keys = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, name, secret FROM provider_api_keys \
             WHERE provider_id = ? AND enabled = 1 \
             ORDER BY id",
        )
        .bind(target.provider_id)
        .fetch_all(&state.pool)
        .await?;
        keys_by_provider.insert(target.provider_id, keys);
    }

    let mut cooldowns = state.provider_key_cooldown.lock().await;
    let now = Instant::now();
    cooldowns.retain(|_, until| *until > now);
    let mut cursors = state.provider_key_cursor.lock().await;
    let mut expanded = Vec::new();
    for target in targets {
        let keys = keys_by_provider
            .get(&target.provider_id)
            .cloned()
            .unwrap_or_default();
        if keys.is_empty() {
            expanded.push(target);
            continue;
        }

        let available = keys
            .iter()
            .filter(|(id, _, _)| !cooldowns.contains_key(id))
            .cloned()
            .collect::<Vec<_>>();
        let mut keys = if available.is_empty() {
            keys
        } else {
            available
        };
        if let Some(session_id) = session_id {
            let mut ranked = keys
                .drain(..)
                .map(|key| {
                    (
                        session_provider_key_hash(session_id, target.provider_id, key.0),
                        key,
                    )
                })
                .collect::<Vec<_>>();
            ranked
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.0.cmp(&right.1.0)));
            keys.extend(ranked.into_iter().map(|(_, key)| key));
        } else {
            let cursor = cursors.entry(target.provider_id).or_default();
            let offset = *cursor % keys.len();
            keys.rotate_left(offset);
            *cursor = cursor.wrapping_add(1);
        }

        for (key_id, key_name, secret) in keys {
            let mut candidate = target.clone();
            candidate.api_key = Some(secret);
            candidate.provider_api_key_id = Some(key_id);
            candidate.provider_api_key_name = Some(key_name);
            expanded.push(candidate);
        }
    }

    let last_index = expanded.len().saturating_sub(1);
    for (index, target) in expanded.iter_mut().enumerate() {
        target.auth_retryable = index < last_index;
    }
    Ok(expanded)
}

pub(super) async fn order_targets(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    mut targets: Vec<RouteTarget>,
    session_id: Option<&str>,
) -> AppResult<Vec<RouteTarget>> {
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let session_id = session_id.map(str::trim).filter(|value| !value.is_empty());
    let runtime_metrics = match strategy {
        RouteStrategy::LatencyOptimized | RouteStrategy::LeastUsed => {
            runtime_target_metrics(state, &targets).await?
        }
        RouteStrategy::Priority
        | RouteStrategy::Weighted
        | RouteStrategy::RoundRobin
        | RouteStrategy::CostOptimized => HashMap::new(),
    };
    if let Some(session_id) = session_id {
        if strategy == RouteStrategy::Priority {
            targets.sort_by_key(|target| (target.priority, target.id));
        }
        order_targets_for_session(session_id, route_id, strategy, &mut targets);
    } else {
        match strategy {
            RouteStrategy::Priority => {
                targets.sort_by_key(|target| (target.priority, target.id));
            }
            RouteStrategy::Weighted => {
                let mut rng = rand::thread_rng();
                let mut selected = Vec::with_capacity(targets.len());
                while !targets.is_empty() {
                    let weights = targets
                        .iter()
                        .map(|target| target.weight.max(1) as u32)
                        .collect::<Vec<_>>();
                    let index = WeightedIndex::new(&weights)
                        .map(|distribution| distribution.sample(&mut rng))
                        .unwrap_or(0);
                    selected.push(targets.remove(index));
                }
                targets = selected;
            }
            RouteStrategy::RoundRobin => {
                let mut cursors = state.round_robin.lock().await;
                let cursor = cursors.entry(route_id).or_default();
                let offset = *cursor % targets.len();
                targets.rotate_left(offset);
                *cursor = cursor.wrapping_add(1);
            }
            RouteStrategy::CostOptimized
            | RouteStrategy::LatencyOptimized
            | RouteStrategy::LeastUsed => {}
        }
    }
    sort_targets_by_strategy_metrics(strategy, &mut targets, &runtime_metrics);
    // Prefer healthy model targets before falling back to provider-level
    // isolation. A model-specific 429 should not hide healthy sibling models.
    let mut target_cooldowns = state.target_cooldown.lock().await;
    let now = Instant::now();
    target_cooldowns.retain(|_, until| *until > now);
    if targets.iter().any(|target| {
        !target_cooldowns.contains_key(&(target.provider_id, target.upstream_model.clone()))
    }) {
        targets.retain(|target| {
            !target_cooldowns.contains_key(&(target.provider_id, target.upstream_model.clone()))
        });
    }
    drop(target_cooldowns);

    // Prefer providers outside their runtime cooldown window. If every
    // candidate is cooling, keep them all so an all-cooling route still has a
    // chance to recover instead of failing before it reaches the upstream.
    let mut provider_cooldowns = state.provider_cooldown.lock().await;
    let now = Instant::now();
    provider_cooldowns.retain(|_, until| *until > now);
    if targets
        .iter()
        .any(|target| !provider_cooldowns.contains_key(&target.provider_id))
    {
        targets.retain(|target| !provider_cooldowns.contains_key(&target.provider_id));
    }
    drop(provider_cooldowns);

    // Keep strategy order within each health group, but try explicitly failed
    // providers last. Unknown health stays ahead of failed providers so a
    // previously-tested outage does not permanently suppress a recovery.
    targets.sort_by_key(|target| provider_health_rank(target.provider_health));
    expand_target_provider_keys(state, targets, session_id).await
}
