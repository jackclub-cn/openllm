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

pub(super) async fn resolve_route(
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
        let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
            strategy: route.strategy,
            targets,
            barrel: Some(barrel),
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
    match resolve_route(state, model, endpoint).await {
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
) -> Duration {
    let base = retry_after.unwrap_or_else(|| match status {
        Some(StatusCode::TOO_MANY_REQUESTS) => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    });
    let exponent = failures.saturating_sub(1).min(4);
    base.saturating_mul(1u32 << exponent)
        .min(MAX_PROVIDER_COOLDOWN)
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

pub(super) fn target_cooldown(status: StatusCode, retry_after: Option<Duration>) -> Duration {
    let base = retry_after.unwrap_or_else(|| match status {
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    });
    base.min(MAX_TARGET_COOLDOWN)
}

pub(super) async fn mark_provider_error(
    state: &AppState,
    provider_id: i64,
    status: Option<StatusCode>,
    retry_after: Option<Duration>,
) {
    let failures = {
        let mut streaks = state.provider_failure_streak.lock().await;
        let failures = streaks.entry(provider_id).or_default();
        *failures = failures.saturating_add(1);
        *failures
    };
    state.provider_cooldown.lock().await.insert(
        provider_id,
        Instant::now() + provider_cooldown(status, failures, retry_after),
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
        now + target_cooldown(status, retry_after),
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
        if status != StatusCode::TOO_MANY_REQUESTS
            || active_model_cooldowns >= PROVIDER_RATE_LIMIT_MODEL_THRESHOLD
        {
            mark_provider_error(state, target.provider_id, Some(status), retry_after).await;
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
    Ok(reqwest::Response),
    /// The upstream failed; the status, headers and body are already read.
    Error {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
}

pub(super) async fn send_provider_request(
    state: &AppState,
    target: &RouteTarget,
    request: RequestBuilder,
) -> AppResult<reqwest::Response> {
    match request.send().await {
        Ok(response) => Ok(response),
        Err(error) => {
            mark_provider_error(state, target.provider_id, None, None).await;
            mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
            Err(AppError::Upstream(format!(
                "{} request failed: {error}",
                target.provider_name
            )))
        }
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
    body: &mut Value,
    build: F,
) -> AppResult<UpstreamAttempt>
where
    F: Fn(&Value) -> AppResult<RequestBuilder>,
{
    let mut response = send_provider_request(state, target, build(body)?).await?;
    let mut transient_retry_used = false;

    loop {
        let status = response.status();
        if status.is_success() {
            return Ok(UpstreamAttempt::Ok(response));
        }
        let headers = response.headers().clone();
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;

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
            response = send_provider_request(state, target, build(body)?).await?;
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
            response = send_provider_request(state, target, build(body)?).await?;
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
