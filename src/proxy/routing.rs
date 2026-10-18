use super::*;

use crate::allocation::PoolDecision;

#[cfg(test)]
#[path = "../../tests/unit/scoring.rs"]
mod scoring_tests;
use crate::state::{
    MAX_SLOW_TARGET_HOLD, PROVIDER_PROBE_LEASE, SLOW_TARGET_STREAK_WINDOW, provider_open_threshold,
    slow_target_threshold_ms,
};

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
        let mut barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        crate::registry::apply_route_capability_overrides(
            &mut barrel,
            route.context_override,
            route.output_override,
        );
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
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
        strategy: "priority".to_string(),
        targets,
        barrel: Some(barrel),
        all_targets_busy_wait_seconds: 0,
        retry_jitter_ms: 250,
        max_waiting_requests: 32,
        keepalive_during_wait: true,
    })
}

pub async fn diagnose_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    diagnose_route_for_key(state, model, endpoint, session_id, None).await
}

/// Diagnoses a route as a specific gateway API key would experience it.
///
/// `api_key_id` only affects the scoring of per-key pool shares; every other
/// signal is key-independent. The whole preview performs no upstream request.
pub async fn diagnose_route_for_key(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<RouteDiagnoseView> {
    let session_id = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(SESSION_ID_MAX_CHARS).collect::<String>());
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, model_pattern,
               CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END AS strategy,
               enabled, context_override, output_override,
               all_targets_busy_wait_seconds, retry_jitter_ms,
               max_waiting_requests, keepalive_during_wait,
               created_at, updated_at
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
                api_key_id,
            )
            .await;
        }
        disabled_match.get_or_insert(route);
    }
    let direct =
        diagnose_direct_route(state, model, endpoint, session_id.as_deref(), api_key_id).await?;
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
            api_key_id,
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
    api_key_id: Option<i64>,
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
               p.last_test_ok AS provider_health,
               p.max_concurrency,
               pm.max_concurrency AS model_max_concurrency
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
    let mut barrel = crate::registry::barrel_for_targets(&state.pool, &eligible_pairs).await?;
    crate::registry::apply_route_capability_overrides(
        &mut barrel,
        route.context_override,
        route.output_override,
    );
    let resolved = route_enabled && !eligible_pairs.is_empty();
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(
            state,
            Some(route.id),
            &route.strategy,
            &rows,
            &diagnosed,
            session_id,
            api_key_id,
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
        strategy: Some(route.strategy.clone()),
        message,
        barrel: barrel.capabilities,
        barrel_incomplete: barrel.incomplete,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        candidates: diagnostic_candidates(
            state,
            Some(route.id),
            &route.strategy,
            &rows,
            &diagnosed,
            session_id,
            api_key_id,
        )
        .await?,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

pub(super) async fn diagnose_direct_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
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
               p.last_test_ok AS provider_health,
               p.max_concurrency,
               pm.max_concurrency AS model_max_concurrency
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
            api_key_id,
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
               p.last_test_ok AS provider_health,
               p.max_concurrency,
               pm.max_concurrency AS model_max_concurrency
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
            api_key_id,
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
        api_key_id,
    )
    .await
}

/// The diagnostic paths thread the request context explicitly rather than in a
/// struct: they are cold paths, and a struct would only hide which signal each
/// caller supplies.
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_direct_diagnosis(
    state: &AppState,
    model: &str,
    endpoint: &str,
    match_type: &str,
    rows: Vec<DiagnosticTargetRow>,
    diagnosed: Vec<(RouteDiagnoseTarget, bool)>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
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
            api_key_id,
        )
        .await?
    } else {
        None
    };
    let candidates = diagnostic_candidates(
        state,
        None,
        RouteStrategy::Priority.as_str(),
        &rows,
        &diagnosed,
        session_id,
        api_key_id,
    )
    .await?;

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
        candidates,
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
    max_concurrency: Option<i64>,
    model_max_concurrency: Option<i64>,
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

/// Builds the routing view of one diagnostic row, so the session ordering and
/// the explainable candidate list score exactly the same target.
fn diagnostic_route_target(row: &DiagnosticTargetRow, route_id: Option<i64>) -> RouteTarget {
    RouteTarget {
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
        max_concurrency: row.max_concurrency,
        queue_timeout_seconds: None,
        model_max_concurrency: row.model_max_concurrency,
        model_queue_timeout_seconds: None,
        provider_health: row.provider_health,
        upstream_model: row.upstream_model.clone(),
        weight: row.target_weight,
        priority: row.target_priority,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        provider_key_max_concurrency: None,
        provider_key_requests_per_minute: None,
        provider_key_queue_limit: None,
        provider_key_queue_wait_ms: None,
        auth_retryable: false,
    }
}

/// Scores every candidate of a diagnosis, in the order the gateway would try
/// them, and appends the ineligible ones with their exclusion reason.
///
/// Makes no upstream request: the score comes from stored health, recent usage
/// rows, live concurrency slots and the pool window.
pub(super) async fn diagnostic_candidates(
    state: &AppState,
    route_id: Option<i64>,
    strategy: &str,
    rows: &[DiagnosticTargetRow],
    diagnosed: &[(RouteDiagnoseTarget, bool)],
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<Vec<RouteDiagnoseCandidate>> {
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let eligible = rows
        .iter()
        .zip(diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| diagnostic_route_target(row, route_id))
        .collect::<Vec<_>>();
    let route_id = route_id.unwrap_or(-1);
    let affinity = session_affinity_context(state, route_id, session_id, api_key_id, false).await;
    let assessments =
        assess_targets(state, strategy, &eligible, api_key_id, affinity.as_ref()).await?;
    let ordered = order_targets_with_context(
        state,
        route_id,
        strategy.as_str(),
        eligible.clone(),
        session_id,
        api_key_id,
        affinity.as_ref(),
    )
    .await?;
    let mut candidates = ordered
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let assessment = assessments
                .get(&(target.provider_id, target.upstream_model.clone()))
                .cloned()
                .unwrap_or_else(|| TargetAssessment::new(Vec::new()));
            candidate_view(index + 1, target, &assessment)
        })
        .collect::<Vec<_>>();
    let ordered_keys = ordered
        .iter()
        .map(|target| (target.provider_id, target.upstream_model.clone()))
        .collect::<HashSet<_>>();
    for target in &eligible {
        if ordered_keys.contains(&(target.provider_id, target.upstream_model.clone())) {
            continue;
        }
        let assessment = assessments
            .get(&(target.provider_id, target.upstream_model.clone()))
            .cloned()
            .unwrap_or_else(|| TargetAssessment::new(Vec::new()));
        let mut candidate = candidate_view(0, target, &assessment);
        if candidate.exclusion.is_none() {
            candidate.exclusion = Some("当前运行时状态使该候选暂不可用".to_string());
        }
        candidate.eligible = false;
        candidate.deprioritized = true;
        candidates.push(candidate);
    }
    // Rows that fail the eligibility pre-checks can never be tried, so they are
    // listed with the reason instead of silently disappearing from the preview.
    for (row, (target, eligible)) in rows.iter().zip(diagnosed) {
        if *eligible {
            continue;
        }
        candidates.push(RouteDiagnoseCandidate {
            order: 0,
            provider_id: row.provider_id,
            provider_name: row.provider_name.clone(),
            upstream_model: row.upstream_model.clone(),
            score: 0.0,
            eligible: false,
            exclusion: Some(target.reason.clone()),
            deprioritized: true,
            failure_scope: None,
            failure_retry: None,
            failure_confidence: None,
            failure_reason_code: None,
            failure_observed_seconds_ago: None,
            factors: Vec::new(),
        });
    }
    let now = Instant::now();
    let target_failure_meta = {
        let active = state
            .target_cooldown
            .lock()
            .await
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();
        let mut metadata = state.target_failure_meta.lock().await;
        metadata.retain(|key, _| active.contains(key));
        metadata.clone()
    };
    let provider_failure_meta = {
        let active = state
            .provider_cooldown
            .lock()
            .await
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(provider_id, _)| *provider_id)
            .collect::<HashSet<_>>();
        let mut metadata = state.provider_failure_meta.lock().await;
        metadata.retain(|provider_id, _| active.contains(provider_id));
        metadata.clone()
    };
    for candidate in &mut candidates {
        let target_key = (candidate.provider_id, candidate.upstream_model.clone());
        let metadata = target_failure_meta
            .get(&target_key)
            .or_else(|| provider_failure_meta.get(&candidate.provider_id));
        if let Some(metadata) = metadata {
            candidate.failure_scope = Some(metadata.scope);
            candidate.failure_retry = Some(metadata.retry);
            candidate.failure_confidence = Some(metadata.confidence);
            candidate.failure_reason_code = Some(metadata.reason_code);
            candidate.failure_observed_seconds_ago = Some(metadata.observed_at.elapsed().as_secs());
        }
    }
    // The request path removes excluded candidates before ordering, so the
    // preview must not show one ahead of a candidate it would actually use.
    // The sort is stable, so the strategy's own order is preserved inside each
    // group.
    candidates.sort_by_key(|candidate| !candidate.eligible);
    for (index, candidate) in candidates.iter_mut().enumerate() {
        candidate.order = index + 1;
    }
    Ok(candidates)
}
pub(super) async fn diagnostic_runtime_targets(
    state: &AppState,
    route_id: Option<i64>,
    strategy: &str,
    rows: &[DiagnosticTargetRow],
    diagnosed: &[(RouteDiagnoseTarget, bool)],
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<Option<Vec<RouteDiagnoseRuntimeTarget>>> {
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let targets = rows
        .iter()
        .zip(diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| diagnostic_route_target(row, route_id))
        .collect::<Vec<_>>();
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let metrics = runtime_target_metrics(
        state,
        &targets,
        strategy == RouteStrategy::P95LatencyOptimized,
    )
    .await?;
    let route_id = route_id.unwrap_or(-1);
    let affinity =
        session_affinity_context(state, route_id, Some(session_id), api_key_id, false).await;
    let ordered = order_targets_with_context(
        state,
        route_id,
        strategy.as_str(),
        targets,
        Some(session_id),
        api_key_id,
        affinity.as_ref(),
    )
    .await?;
    let quota_states = crate::quota::target_scope_states(state, &ordered).await;
    Ok(Some(
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                let metric = metrics.get(&(target.provider_id, target.upstream_model.clone()));
                let (input_cost_per_million, output_cost_per_million) = target_cost_prices(&target);
                let session_pinned = affinity
                    .as_ref()
                    .and_then(|affinity| affinity.pinned.as_ref())
                    .is_some_and(|pinned| same_affinity_target(&target, pinned));
                let decision_reason = if index == 0 && session_pinned {
                    "session_affinity".to_string()
                } else {
                    target_decision_reason(
                        strategy,
                        &target,
                        metric,
                        quota_states.get(&crate::quota::quota_target_key(&target)),
                        index == 0,
                    )
                    .to_string()
                };
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
    let jittered = delay.saturating_mul(100u64.saturating_add(jitter_percent)) / 100;
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

    if let Some(value) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Ok(seconds) = value.parse::<u64>() {
            return bounded_retry_after(Duration::from_secs(seconds), MAX_UPSTREAM_RETRY_AFTER);
        }

        if let Ok(deadline) = httpdate::parse_http_date(value)
            && let Ok(duration) = deadline.duration_since(std::time::SystemTime::now())
        {
            return bounded_retry_after(duration, MAX_UPSTREAM_RETRY_AFTER);
        }
    }

    retry_after_from_reset_headers(headers)
}

/// Parses the rate-limit reset headers used by common OpenAI and Anthropic
/// compatible upstreams. When several windows are present, wait for the latest
/// one so the next attempt does not immediately hit another active limit.
fn retry_after_from_reset_headers(headers: &HeaderMap) -> Option<Duration> {
    const RESET_AFTER_HEADERS: [&str; 2] = ["x-ratelimit-reset-after", "ratelimit-reset-after"];
    const RESET_DURATION_HEADERS: [&str; 2] =
        ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"];
    const RESET_TIMESTAMP_HEADERS: [&str; 3] = [
        "anthropic-ratelimit-unified-reset",
        "x-ratelimit-reset",
        "ratelimit-reset",
    ];

    let mut latest = None;
    for name in RESET_AFTER_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_after)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    for name in RESET_DURATION_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_duration)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    for name in RESET_TIMESTAMP_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_timestamp)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    latest.and_then(|duration| bounded_retry_after(duration, MAX_UPSTREAM_RETRY_AFTER))
}

fn parse_reset_after(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

fn parse_reset_duration(value: &str) -> Option<Duration> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(seconds) = parse_reset_after(&value) {
        return Some(seconds);
    }

    let mut total = Duration::ZERO;
    let mut rest = value.as_str();
    let mut parsed_any = false;
    while !rest.is_empty() {
        let number_end = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(rest.len());
        if number_end == 0 {
            return None;
        }
        let number = rest[..number_end].parse::<f64>().ok()?;
        rest = &rest[number_end..];

        let unit_end = rest
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(rest.len());
        if unit_end == 0 {
            return None;
        }
        let multiplier = match &rest[..unit_end] {
            "ns" => 1e-9,
            "us" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        let seconds = number * multiplier;
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        total = total.saturating_add(Duration::try_from_secs_f64(seconds).ok()?);
        rest = &rest[unit_end..];
        parsed_any = true;
    }
    parsed_any.then_some(total)
}

fn parse_reset_timestamp(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(raw) = value.parse::<i64>() {
        // Unix reset timestamps may be seconds or milliseconds.
        let seconds = if raw > 10_000_000_000 {
            raw / 1000
        } else {
            raw
        };
        let now = Utc::now().timestamp();
        return (seconds > now).then(|| Duration::from_secs((seconds - now) as u64));
    }
    let deadline = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    let duration = deadline
        .with_timezone(&Utc)
        .signed_duration_since(Utc::now())
        .to_std()
        .ok()?;
    Some(duration)
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

pub(super) fn target_sink_key(target: &RouteTarget) -> crate::state::TargetSinkKey {
    (
        target.provider_id,
        target.provider_api_key_id,
        target.upstream_model.clone(),
    )
}

fn target_sink_ttl(retry_after: Option<Duration>) -> Duration {
    const DEFAULT_SINK: Duration = Duration::from_secs(30);
    const MIN_SINK: Duration = Duration::from_secs(1);
    const MAX_SINK: Duration = Duration::from_secs(5 * 60);
    retry_after
        .unwrap_or(DEFAULT_SINK)
        .clamp(MIN_SINK, MAX_SINK)
}

pub(super) async fn mark_target_sink(
    state: &AppState,
    target: &RouteTarget,
    reason_code: &'static str,
    retry_after: Option<Duration>,
) {
    let now = Instant::now();
    let key = target_sink_key(target);
    let deadline = now + target_sink_ttl(retry_after);
    state
        .target_sink
        .lock()
        .await
        .entry(key.clone())
        .and_modify(|entry| {
            entry.until = entry.until.max(deadline);
            entry.reason_code = reason_code;
            entry.observed_at = now;
        })
        .or_insert(crate::state::TargetSinkEntry {
            until: deadline,
            reason_code,
            observed_at: now,
        });
    state.mark_target_sink_dirty(&key);
}

async fn clear_target_sinks_for_provider_model(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
) {
    let removed = {
        let mut sinks = state.target_sink.lock().await;
        let keys = sinks
            .keys()
            .filter(|(target_provider_id, _, model)| {
                *target_provider_id == provider_id && model == upstream_model
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            sinks.remove(key);
        }
        keys
    };
    for key in &removed {
        state.mark_target_sink_dirty(key);
    }
}

pub(crate) async fn clear_target_sinks_for_route(state: &AppState, route_id: i64) -> usize {
    let pairs = sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, upstream_model FROM route_targets WHERE route_id = ?",
    )
    .bind(route_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let pairs = pairs.into_iter().collect::<HashSet<_>>();
    let removed = {
        let mut sinks = state.target_sink.lock().await;
        let keys = sinks
            .keys()
            .filter(|(provider_id, _, model)| pairs.contains(&(*provider_id, model.clone())))
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            sinks.remove(key);
        }
        keys
    };
    for key in &removed {
        state.mark_target_sink_dirty(key);
    }
    removed.len()
}

pub(super) async fn target_sink_active(state: &AppState, target: &RouteTarget) -> bool {
    let now = Instant::now();
    let mut sinks = state.target_sink.lock().await;
    sinks.retain(|_, entry| entry.until > now);
    sinks.contains_key(&target_sink_key(target))
}

pub(super) async fn busy_waitable_error(
    state: &AppState,
    target: &RouteTarget,
    error: &AppError,
) -> bool {
    match error {
        AppError::Upstream(_) => true,
        AppError::UpstreamStatus { status, .. } => match *status {
            StatusCode::TOO_MANY_REQUESTS => target_sink_active(state, target).await,
            StatusCode::REQUEST_TIMEOUT => true,
            status if status.as_u16() == 425 => true,
            status if status.is_server_error() => true,
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn busy_wait_retry_after(error: &AppError) -> Option<Duration> {
    match error {
        AppError::UpstreamStatus { retry_after, .. } => *retry_after,
        _ => None,
    }
}

pub(super) fn busy_wait_delay(
    jitter_ms: i64,
    retry_after: Option<Duration>,
    round: u32,
) -> Duration {
    const MIN_DELAY: Duration = Duration::from_millis(50);
    const MAX_DELAY: Duration = Duration::from_secs(5);
    let jitter_ms = u64::try_from(jitter_ms.max(0)).unwrap_or(0);
    let jitter = if jitter_ms == 0 {
        Duration::ZERO
    } else {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos() as u64)
            .unwrap_or(0);
        Duration::from_millis(nanos % jitter_ms.saturating_add(1))
    };
    let hinted = retry_after.unwrap_or_default().min(MAX_DELAY);
    let backoff = MIN_DELAY
        .checked_mul(1u32.checked_shl(round.min(16)).unwrap_or(u32::MAX))
        .unwrap_or(MAX_DELAY)
        .min(MAX_DELAY);
    (backoff.max(hinted) + jitter).min(MAX_DELAY)
}

/// Minimum probe reservation margin, added on top of a configured provider
/// header timeout so the probe lease cannot expire while the upstream is still
/// allowed to answer.
const PROVIDER_PROBE_TIMEOUT_MARGIN: Duration = Duration::from_secs(30);

pub(super) fn provider_probe_lease(timeout_seconds: Option<i64>) -> Duration {
    let configured = timeout_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    configured
        .map(|timeout| {
            PROVIDER_PROBE_LEASE.max(timeout.saturating_add(PROVIDER_PROBE_TIMEOUT_MARGIN))
        })
        .unwrap_or(PROVIDER_PROBE_LEASE)
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
    let now = Instant::now();
    // A probe that just failed is no longer in flight; the cooldown below
    // decides when the next recovery probe may be admitted. Whether one was in
    // flight also tells the trip counter that a HALF_OPEN probe re-opened the
    // circuit rather than a fresh streak crossing the threshold.
    let probe_was_in_flight = state
        .provider_probe
        .lock()
        .await
        .remove(&provider_id)
        .is_some();
    let threshold = provider_open_threshold();
    let (failures, opened, cooldown) = {
        // Keep the streak and deadline under one lock so a burst of requests
        // that failed just before the circuit opened cannot keep extending the
        // same outage. A half-open probe is the one exception: its failure must
        // re-open the circuit with the next escalated cooldown.
        let mut cooldowns = state.provider_cooldown.lock().await;
        let mut streaks = state.provider_failure_streak.lock().await;
        let current = streaks.get(&provider_id).copied().unwrap_or(0);
        if threshold > 0
            && !probe_was_in_flight
            && current >= threshold
            && cooldowns
                .get(&provider_id)
                .is_some_and(|until| *until > now)
        {
            tracing::debug!(
                provider_id,
                failure_streak = current,
                "ignoring a follower failure while the provider circuit is open"
            );
            return;
        }

        let failures = streaks.entry(provider_id).or_default();
        *failures = failures.saturating_add(1);
        let opened = threshold > 0
            && *failures >= threshold
            && (*failures - 1 < threshold || probe_was_in_flight);
        let cooldown =
            provider_cooldown(status, *failures, retry_after, configured_cooldown_seconds);
        let deadline = now + cooldown;
        cooldowns
            .entry(provider_id)
            .and_modify(|until| *until = (*until).max(deadline))
            .or_insert(deadline);
        (*failures, opened, cooldown)
    };
    if opened {
        *state
            .provider_circuit_trips
            .lock()
            .await
            .entry(provider_id)
            .or_default() += 1;
        tracing::warn!(
            provider_id,
            failures,
            "provider circuit opened after repeated failures; short-circuiting until it recovers"
        );
    }
    // Mirror the trip so a restart still knows this provider is unhealthy.
    state.mark_provider_resilience_dirty(provider_id);
    if opened {
        let _ = state
            .circuit_events
            .send(crate::state::ProviderCircuitEvent {
                provider_id,
                state: "open",
                reason: if probe_was_in_flight {
                    "probe_failed"
                } else {
                    "failure_threshold"
                },
                failure_streak: failures,
                cooldown_seconds: cooldown.as_secs(),
            });
    }
}

pub(super) async fn mark_target_error(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    retry_after: Option<Duration>,
) -> usize {
    let now = Instant::now();
    let active = {
        let mut cooldowns = state.target_cooldown.lock().await;
        cooldowns.retain(|_, until| *until > now);
        let key = (target.provider_id, target.upstream_model.clone());
        let deadline = now + target_cooldown(status, retry_after, target.cooldown_seconds);
        cooldowns
            .entry(key)
            .and_modify(|until| *until = (*until).max(deadline))
            .or_insert(deadline);
        cooldowns
            .keys()
            .filter(|(provider_id, _)| *provider_id == target.provider_id)
            .count()
    };
    state.mark_target_resilience_dirty(target.provider_id, &target.upstream_model);
    active
}

pub(super) async fn mark_target_success(state: &AppState, provider_id: i64, upstream_model: &str) {
    let removed = state
        .target_cooldown
        .lock()
        .await
        .remove(&(provider_id, upstream_model.to_string()));
    state
        .target_failure_meta
        .lock()
        .await
        .remove(&(provider_id, upstream_model.to_string()));
    if removed.is_some() {
        state.mark_target_resilience_dirty(provider_id, upstream_model);
    }
    clear_target_sinks_for_provider_model(state, provider_id, upstream_model).await;
}

/// Records a successful request's first-token latency as a soft routing signal.
///
/// A 200 response proves the provider is reachable, so this must not open the
/// failure circuit or a hard model cooldown. The sample is only consulted by
/// `latency_optimized` routes, where it makes a target lose to healthy siblings
/// for a bounded period. That keeps a quota-drained upstream from attracting
/// every following request on a latency-first route while still eventually
/// probing it again, without touching the order configured for every other
/// strategy.
pub(super) async fn record_target_latency_success(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
    first_token_ms: Option<i64>,
) {
    let threshold_ms = slow_target_threshold_ms();
    if threshold_ms <= 0 {
        return;
    }
    let Some(first_token_ms) = first_token_ms.filter(|latency| *latency >= threshold_ms) else {
        return;
    };

    let now = Instant::now();
    let mut targets = state.target_slow.lock().await;
    targets.retain(|_, entry| {
        entry.until.is_some_and(|until| until > now)
            || entry.last_slow_at.is_some_and(|last| {
                now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW
            })
    });

    let entry = targets
        .entry((provider_id, upstream_model.to_string()))
        .or_default();
    let same_streak = entry
        .last_slow_at
        .is_some_and(|last| now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW);
    entry.slow_streak = if same_streak {
        entry.slow_streak.saturating_add(1)
    } else {
        1
    };
    entry.last_slow_at = Some(now);
    entry.last_first_token_ms = first_token_ms;
    let hold = slow_target_hold(first_token_ms, threshold_ms, entry.slow_streak);
    entry.until = Some(now + hold);

    tracing::warn!(
        provider_id,
        model = upstream_model,
        first_token_ms,
        threshold_ms,
        slow_streak = entry.slow_streak,
        hold_seconds = hold.as_secs(),
        "successful upstream response was slow; recorded for latency-optimized routes"
    );
}

fn slow_target_hold(first_token_ms: i64, threshold_ms: i64, slow_streak: u32) -> Duration {
    const BASE_HOLD: Duration = Duration::from_secs(30);
    const MAX_OBSERVED_ADDITION: Duration = Duration::from_secs(120);

    let observed_addition = Duration::from_millis(
        first_token_ms
            .saturating_sub(threshold_ms)
            .max(0)
            .unsigned_abs()
            .min(MAX_OBSERVED_ADDITION.as_millis() as u64)
            / 2,
    );
    let multiplier = 1u32 << slow_streak.saturating_sub(1).min(3);
    (BASE_HOLD + observed_addition)
        .saturating_mul(multiplier)
        .min(MAX_SLOW_TARGET_HOLD)
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
    record_transport_failure(state, target, "upstream_stream_failed", message).await;
}

/// Records a transport fault that happened before an upstream response was
/// received. Transport failures are provider-wide by nature, but the target
/// still gets a model cooldown so the request path does not immediately choose
/// the same broken connection again.
pub(super) async fn record_transport_failure(
    state: &AppState,
    target: &RouteTarget,
    reason_code: &'static str,
    message: &str,
) -> UpstreamFailure {
    let failure = UpstreamFailure::transport(reason_code);
    count_failure_classification(state, failure).await;
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        provider_key_id = target.provider_api_key_id,
        scope = failure.scope.as_str(),
        retry = failure.retry.as_str(),
        confidence = failure.confidence.as_str(),
        reason = failure.reason_code,
        %message,
        "classified upstream transport failure"
    );
    mark_provider_error(
        state,
        target.provider_id,
        None,
        None,
        target.cooldown_seconds,
    )
    .await;
    mark_target_error(state, target, StatusCode::BAD_GATEWAY, None).await;
    let metadata = failure.metadata();
    state.target_failure_meta.lock().await.insert(
        (target.provider_id, target.upstream_model.clone()),
        metadata.clone(),
    );
    state
        .provider_failure_meta
        .lock()
        .await
        .insert(target.provider_id, metadata);
    failure
}

async fn count_failure_classification(state: &AppState, failure: UpstreamFailure) {
    *state
        .failure_classifications
        .lock()
        .await
        .entry((
            failure.scope.as_str().to_string(),
            failure.retry.as_str().to_string(),
            failure.reason_code.to_string(),
        ))
        .or_default() += 1;
}

pub(super) async fn record_upstream_failure(
    state: &AppState,
    target: &RouteTarget,
    status: StatusCode,
    response_headers: &HeaderMap,
    message: &str,
) -> UpstreamFailure {
    let retry_after = retry_after_from_headers(response_headers);
    let failure = classify_upstream_failure(target, status, message.as_bytes());
    crate::quota::observe_response_headers(state, target, response_headers).await;
    if status == StatusCode::TOO_MANY_REQUESTS && failure.reason_code == "quota_exhausted" {
        crate::quota::observe_runtime_quota_failure(state, target, retry_after).await;
    }
    tracing::warn!(
        provider = %target.provider_name,
        model = %target.upstream_model,
        provider_key_id = target.provider_api_key_id,
        status = status.as_u16(),
        scope = failure.scope.as_str(),
        retry = failure.retry.as_str(),
        confidence = failure.confidence.as_str(),
        reason = failure.reason_code,
        "classified upstream failure"
    );
    count_failure_classification(state, failure).await;
    if failure.should_sink() {
        mark_target_sink(state, target, failure.reason_code, retry_after).await;
    }
    let metadata = failure.metadata();
    let mut provider_rested = false;
    if failure.should_rest_target() {
        let active_model_cooldowns = mark_target_error(state, target, status, retry_after).await;
        state.target_failure_meta.lock().await.insert(
            (target.provider_id, target.upstream_model.clone()),
            metadata.clone(),
        );
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
            provider_rested = true;
        }
    }
    if provider_reachable_status(status) {
        mark_provider_reachable(state, target.provider_id).await;
    }
    if failure.should_rest_credential() {
        if let Some(provider_api_key_id) = target.provider_api_key_id {
            state
                .provider_key_failure_meta
                .lock()
                .await
                .insert(provider_api_key_id, metadata.clone());
        }
        mark_provider_api_key_error(
            state,
            target.provider_api_key_id,
            status,
            &format!("{} returned {}: {}", target.provider_name, status, message),
            retry_after,
        )
        .await;
    }
    if provider_rested {
        state
            .provider_failure_meta
            .lock()
            .await
            .insert(target.provider_id, metadata);
    }
    failure
}

pub(super) fn provider_circuit_should_open(
    status: StatusCode,
    active_model_cooldowns: usize,
) -> bool {
    // A 409 usually describes this request's resource state, not the
    // provider's health, so it should fall through without opening a circuit.
    status != StatusCode::CONFLICT && active_model_cooldowns >= PROVIDER_MODEL_COOLDOWN_THRESHOLD
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
    close_provider_circuit(state, provider_id, "successful_request").await;
}

/// A reachable upstream response proves a half-open probe succeeded even when
/// the status itself is a client error or a rate limit. Close the provider
/// circuit, but leave model and key cooldowns in place.
pub(super) async fn mark_provider_reachable(state: &AppState, provider_id: i64) {
    let probe_was_active = state
        .provider_probe
        .lock()
        .await
        .remove(&provider_id)
        .is_some();
    if !probe_was_active {
        return;
    }
    close_provider_circuit(state, provider_id, "reachable_response").await;
}

async fn close_provider_circuit(state: &AppState, provider_id: i64, reason: &'static str) {
    state.provider_cooldown.lock().await.remove(&provider_id);
    state
        .provider_failure_meta
        .lock()
        .await
        .remove(&provider_id);
    let streak = state
        .provider_failure_streak
        .lock()
        .await
        .remove(&provider_id)
        .unwrap_or(0);
    state.provider_probe.lock().await.remove(&provider_id);
    // A recovery must be persisted too, or a restart would restore the
    // cooldown this success just cleared.
    state.mark_provider_resilience_dirty(provider_id);
    let threshold = provider_open_threshold();
    if threshold > 0 && streak >= threshold {
        tracing::info!(provider_id, reason, "provider circuit closed");
        let _ = state
            .circuit_events
            .send(crate::state::ProviderCircuitEvent {
                provider_id,
                state: "closed",
                reason,
                failure_streak: 0,
                cooldown_seconds: 0,
            });
    }
}

/// Applies the per-provider circuit breaker to one upstream attempt.
///
/// Returns `Ok(())` when the request may try the provider, or `Err(retry_after)`
/// when a hard-open circuit short-circuits it.
///
/// A provider below the configured failure threshold is always allowed: its
/// runtime cooldown only deprioritises it (see `order_targets`), so one
/// transient blip on an otherwise healthy provider cannot turn into an outage.
/// At or above the threshold the provider stays blocked while its cooldown is
/// active, and once the cooldown elapses exactly one request is admitted to
/// probe recovery. That probe is what lets the provider leave the open state:
/// a success clears the streak, a failure re-opens with the escalated cooldown.
pub(super) async fn admit_provider_attempt_with_lease(
    state: &AppState,
    provider_id: i64,
    probe_lease: Duration,
) -> Result<(), Duration> {
    let threshold = provider_open_threshold();
    if threshold == 0 {
        return Ok(());
    }
    let failures = state
        .provider_failure_streak
        .lock()
        .await
        .get(&provider_id)
        .copied()
        .unwrap_or(0);
    if failures < threshold {
        return Ok(());
    }

    let now = Instant::now();
    let open_until = state
        .provider_cooldown
        .lock()
        .await
        .get(&provider_id)
        .copied();
    if let Some(until) = open_until
        && until > now
    {
        return Err(until.saturating_duration_since(now));
    }

    // Cooldown elapsed: half-open. Admit exactly one probe; a probe that is
    // still within its lease means another request is already testing recovery.
    let mut probes = state.provider_probe.lock().await;
    probes.retain(|_, started| now.saturating_duration_since(*started) < probe_lease);
    if probes.contains_key(&provider_id) {
        return Err(probe_lease);
    }
    probes.insert(provider_id, now);
    Ok(())
}

/// Releases a recovery-probe lease without touching the failure streak.
///
/// A half-open provider admits exactly one probe (see [`admit_provider_attempt`])
/// so a burst cannot stampede a provider that is only just recovering. If that
/// admitted attempt is then stopped before it reaches the upstream (today a
/// provider-pool denial), the probe no longer represents anything in flight, so
/// it has to be released here. Leaving it would keep the provider short-circuited
/// for the whole probe lease even though nothing ever tested it.
pub(super) async fn release_provider_probe(state: &AppState, provider_id: i64) {
    state.provider_probe.lock().await.remove(&provider_id);
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
                write!(
                    formatter,
                    "no response headers within {}s",
                    budget.as_secs()
                )
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
        let request = match build() {
            Ok(request) => request,
            Err(error) => {
                // The request could not even be built, so nothing reached the
                // upstream. Hand back a half-open probe the caller admitted
                // instead of holding it for the whole lease; the transport path
                // below already releases it through `mark_provider_error`.
                release_provider_probe(state, target.provider_id).await;
                return Err(error);
            }
        };
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
            Some(budget) => request
                .timeout(budget)
                .send()
                .await
                .map_err(SendError::Http),
            None => request.send().await.map_err(SendError::Http),
        };

        match outcome {
            Ok(response) => {
                crate::quota::observe_response_headers(state, target, response.headers()).await;
                return Ok(response);
            }
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
                record_transport_failure(
                    state,
                    target,
                    "upstream_transport_error",
                    &error.to_string(),
                )
                .await;
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

const PROVIDER_KEY_RPM_WINDOW: Duration = Duration::from_secs(60);

async fn provider_key_rpm_window(
    state: &AppState,
    provider_api_key_id: i64,
) -> (usize, Option<Duration>) {
    let now = Instant::now();
    let mut windows = state.provider_key_rpm.lock().await;
    let starts = windows.entry(provider_api_key_id).or_default();
    while starts
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started) >= PROVIDER_KEY_RPM_WINDOW)
    {
        starts.pop_front();
    }
    let retry_after = starts.front().map(|started| {
        PROVIDER_KEY_RPM_WINDOW.saturating_sub(now.saturating_duration_since(*started))
    });
    (starts.len(), retry_after)
}

async fn check_provider_key_rpm(
    state: &AppState,
    provider_api_key_id: i64,
    limit: usize,
) -> AppResult<()> {
    let (used, retry_after) = provider_key_rpm_window(state, provider_api_key_id).await;
    if used < limit {
        return Ok(());
    }
    Err(AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: format!("provider API key is at its rolling 60-second request limit ({limit})"),
        retry_after: Some(
            retry_after
                .unwrap_or(Duration::from_secs(1))
                .max(Duration::from_millis(1)),
        ),
    })
}

async fn record_provider_key_rpm(
    state: &AppState,
    provider_api_key_id: i64,
    limit: usize,
) -> AppResult<()> {
    let now = Instant::now();
    let mut windows = state.provider_key_rpm.lock().await;
    let starts = windows.entry(provider_api_key_id).or_default();
    while starts
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started) >= PROVIDER_KEY_RPM_WINDOW)
    {
        starts.pop_front();
    }
    if starts.len() >= limit {
        let retry_after = starts
            .front()
            .map(|started| {
                PROVIDER_KEY_RPM_WINDOW.saturating_sub(now.saturating_duration_since(*started))
            })
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_millis(1));
        return Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "provider API key is at its rolling 60-second request limit ({limit})"
            ),
            retry_after: Some(retry_after),
        });
    }
    starts.push_back(now);
    Ok(())
}

async fn acquire_provider_key_slot(
    state: &AppState,
    target: &RouteTarget,
) -> Result<Option<OwnedSemaphorePermit>, AcquireSlotError> {
    let Some(provider_api_key_id) = target.provider_api_key_id else {
        return Ok(None);
    };
    let Some(limit) = target
        .provider_key_max_concurrency
        .filter(|limit| *limit > 0)
    else {
        return Ok(None);
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let semaphore = {
        let mut semaphores = state.provider_key_concurrency.lock().await;
        semaphores
            .entry(provider_api_key_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1))))
            .clone()
    };
    let wait = target
        .provider_key_queue_wait_ms
        .map(|milliseconds| Duration::from_millis(milliseconds.unsigned_abs()))
        .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
    let label = target
        .provider_api_key_name
        .clone()
        .unwrap_or_else(|| format!("provider API key {provider_api_key_id}"));

    if let Ok(permit) = semaphore.clone().try_acquire_owned() {
        return Ok(Some(permit));
    }
    if wait.is_zero() {
        return Err(AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "provider API key {label} is at its concurrency limit ({limit}); retry shortly"
            ),
            retry_after: Some(Duration::from_secs(1)),
        }));
    }

    let _queue_guard = if let Some(queue_limit) = target.provider_key_queue_limit {
        if queue_limit <= 0 {
            return Err(AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!(
                    "provider API key {label} is at its concurrency limit ({limit}) and queueing is disabled"
                ),
                retry_after: Some(Duration::from_secs(1)),
            }));
        }
        let queue_limit = usize::try_from(queue_limit).unwrap_or(usize::MAX);
        let queue = {
            let mut queues = state.provider_key_queue.lock().await;
            queues
                .entry(provider_api_key_id)
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(queue_limit)))
                .clone()
        };
        Some(queue.try_acquire_owned().map_err(|_| {
            AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!(
                    "provider API key {label} queue is full ({queue_limit}); retry shortly"
                ),
                retry_after: Some(Duration::from_secs(1)),
            })
        })?)
    } else {
        None
    };

    acquire_limit_slot(
        semaphore,
        limit,
        wait,
        format!("provider API key {label}"),
        "provider API key concurrency gate closed",
    )
    .await
    .map(Some)
    .map_err(AcquireSlotError::ProviderApiKey)
}

/// Waits for model and provider concurrency slots. Model slots are acquired
/// first so a queue of requests for one hot model cannot occupy every provider
/// slot and starve healthy sibling models. The selected upstream credential is
/// checked before either provider-wide or model-wide gates.
pub(super) async fn acquire_upstream_slots(
    state: &AppState,
    target: &RouteTarget,
) -> Result<Option<UpstreamSlots>, AcquireSlotError> {
    if let (Some(provider_api_key_id), Some(limit)) = (
        target.provider_api_key_id,
        target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0),
    ) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        check_provider_key_rpm(state, provider_api_key_id, limit)
            .await
            .map_err(AcquireSlotError::ProviderApiKey)?;
    }
    let provider_key = acquire_provider_key_slot(state, target).await?;

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
                    format!("{}/{}", target.provider_name, target.upstream_model),
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

    if let (Some(provider_api_key_id), Some(limit)) = (
        target.provider_api_key_id,
        target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0),
    ) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        record_provider_key_rpm(state, provider_api_key_id, limit)
            .await
            .map_err(AcquireSlotError::ProviderApiKey)?;
    }

    if provider_key.is_none() && model.is_none() && provider.is_none() {
        Ok(None)
    } else {
        Ok(Some(UpstreamSlots {
            provider_key,
            provider,
            model,
        }))
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
/// - if an upstream rejects replayed reasoning history or strict tool fields,
///   repair the request once, retry the same target, and persist the learned
///   capability for the provider/model/endpoint scope.
///
/// `body` is the exact body sent upstream, so compatibility retries update the
/// request rather than replaying the rejected payload.
///
/// The argument list is flat on purpose: the callers forward values they already
/// hold, and a parameter struct would only add a construction step at each of
/// the three dispatch sites.
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_provider_request_with_compat_retry<F>(
    state: &AppState,
    target: &RouteTarget,
    api_key_id: Option<i64>,
    request_id: &str,
    endpoint: &str,
    shape: UpstreamShape,
    streamed: bool,
    body: &mut Value,
    build: F,
) -> AppResult<UpstreamAttempt>
where
    F: Fn(&Value) -> AppResult<RequestBuilder>,
{
    // Circuit breaker: short-circuit a hard-open provider here, before any
    // upstream call, so a known-bad provider cannot pin an admission slot for a
    // full header timeout. The fallback loop moves to the next target, and if
    // none can serve, the client gets a fast 503 with a Retry-After.
    let probe_lease = provider_probe_lease(target.timeout_seconds);
    if let Err(retry_after) =
        admit_provider_attempt_with_lease(state, target.provider_id, probe_lease).await
    {
        return Err(AppError::UpstreamStatus {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "{} is short-circuited after repeated failures; retry shortly or use another target",
                target.provider_name
            ),
            retry_after: Some(retry_after.max(Duration::from_secs(1))),
        });
    }

    // Provider quota pool, charged once per attempt and only after every local
    // gate above has passed: charging earlier (in the caller, before the
    // concurrency slots and the circuit breaker) would let traffic the gateway
    // rejects locally burn quota that no upstream call ever consumed, and
    // charging later would race another attempt for the last free slot of the
    // window. A denial here is a routing decision, so the fallback loop simply
    // moves to the next target.
    if let Some((reason, retry_after)) =
        crate::allocation::charge_target_attempt(state, target.provider_id, api_key_id).await
    {
        // The attempt was admitted as a half-open recovery probe above but is
        // denied before any upstream call, so hand the probe back. Otherwise the
        // provider stays "probe in flight" and short-circuits every request for
        // `PROVIDER_PROBE_LEASE` even though nothing was ever tested.
        release_provider_probe(state, target.provider_id).await;
        return Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("excluded provider '{}': {reason}", target.provider_name),
            retry_after: Some(retry_after),
        });
    }
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
    let mut reasoning_retry_used = false;
    let mut strict_tools_retry_used = false;

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
                        record_transport_failure(
                            state,
                            target,
                            "upstream_stream_failed",
                            &error.to_string(),
                        )
                        .await;
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

        if !reasoning_retry_used
            && (status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY)
            && compat_thinking_history_error(status, &response_body)
        {
            let repaired = match shape {
                UpstreamShape::Responses => {
                    super::compat::normalize_responses_reasoning_history(body)
                }
                UpstreamShape::Chat => super::compat::normalize_chat_reasoning_history(body),
                UpstreamShape::Anthropic => {
                    super::compat::normalize_anthropic_thinking_history(body)
                }
            };
            if repaired > 0 {
                reasoning_retry_used = true;
                tracing::warn!(
                    provider = %target.provider_name,
                    model = %target.upstream_model,
                    request_id,
                    endpoint,
                    repaired,
                    "upstream rejected reasoning history; retrying with repaired history"
                );
                crate::capabilities::record_capability(
                    state,
                    target.provider_id,
                    &target.upstream_model,
                    endpoint,
                    crate::capabilities::CAPABILITY_REASONING_HISTORY,
                    true,
                    crate::capabilities::SOURCE_RUNTIME,
                    "exact",
                )
                .await;
                response = UpstreamResponse::new(
                    send_provider_request(state, target, streamed, || build(body)).await?,
                );
                continue;
            }
        }

        if !strict_tools_retry_used
            && matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            )
            && super::compat::upstream_rejects_strict_tools(&response_body)
            && let Some(compat_body) = super::compat::strip_strict_tools(body)
        {
            strict_tools_retry_used = true;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                endpoint,
                "upstream rejected strict tool fields; retrying without them"
            );
            crate::capabilities::record_capability(
                state,
                target.provider_id,
                &target.upstream_model,
                endpoint,
                crate::capabilities::CAPABILITY_STRICT_TOOLS,
                false,
                crate::capabilities::SOURCE_RUNTIME,
                "exact",
            )
            .await;
            *body = compat_body;
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
    let deadline = Instant::now() + provider_key_cooldown(status, retry_after);
    state
        .provider_key_cooldown
        .lock()
        .await
        .entry(provider_api_key_id)
        .and_modify(|until| *until = (*until).max(deadline))
        .or_insert(deadline);
    state.mark_provider_key_resilience_dirty(provider_api_key_id);
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
    state
        .provider_key_failure_meta
        .lock()
        .await
        .remove(&provider_api_key_id);
    state.mark_provider_key_resilience_dirty(provider_api_key_id);
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
        | RouteStrategy::P95LatencyOptimized
        | RouteStrategy::LeastUsed
        | RouteStrategy::QuotaPaced => {}
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RuntimeTargetMetricRow {
    provider_id: i64,
    upstream_model: String,
    requests: i64,
    prompt_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    avg_latency_ms: Option<f64>,
    /// Nearest-rank 95th percentile first-token (or total) latency.
    p95_latency_ms: Option<f64>,
    /// Rows that finished successfully in the window.
    successes: i64,
    /// Finished rows that failed because of the upstream (5xx, rate limit or a
    /// transport error). Client-side rejections are not counted, so a caller
    /// sending broken requests cannot make a healthy provider look unreliable.
    upstream_faults: i64,
}

impl RuntimeTargetMetricRow {
    /// Recent success rate over the upstream-caused outcomes, or `None` when
    /// there is not enough evidence to judge.
    fn success_rate(&self) -> Option<f64> {
        let sample = self.successes + self.upstream_faults;
        (sample > 0).then(|| self.successes as f64 / sample as f64)
    }

    fn has_cache_evidence(&self) -> bool {
        self.cache_roi().is_some()
    }

    fn cache_hit_rate(&self) -> Option<f64> {
        (self.prompt_tokens > 0 && self.cache_read_tokens > 0)
            .then(|| (self.cache_read_tokens as f64 / self.prompt_tokens as f64).clamp(0.0, 1.0))
    }

    /// Estimates whether recent prompt-cache traffic is likely to be useful.
    ///
    /// A high hit rate means more prompt tokens were served from cache, while
    /// the read/write ratio keeps a target from looking attractive when it
    /// mostly paid to establish a cache that was rarely reused.
    fn cache_roi(&self) -> Option<f64> {
        let reads = self.cache_read_tokens.max(0) as f64;
        if reads <= 0.0 {
            return None;
        }
        let writes = self.cache_write_tokens.max(0) as f64;
        let hit_rate = self.cache_hit_rate()?;
        let reuse_ratio = reads / (reads + writes).max(1.0);
        Some((hit_rate * 0.7 + reuse_ratio * 0.3).clamp(0.0, 1.0))
    }
}

async fn runtime_target_metrics(
    state: &AppState,
    targets: &[RouteTarget],
    include_p95: bool,
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
               SUM(prompt_tokens) AS prompt_tokens,
               SUM(cache_read_tokens) AS cache_read_tokens,
               SUM(cache_write_tokens) AS cache_write_tokens,
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
               ) AS avg_latency_ms,
               NULL AS p95_latency_ms,
               SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS successes,
               SUM(
                   CASE
                       WHEN success = 0
                            AND (status_code >= 500 OR status_code = 429 OR status_code = 0)
                           THEN 1
                       ELSE 0
                   END
               ) AS upstream_faults
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
    let mut metrics = rows
        .into_iter()
        .map(|row| ((row.provider_id, row.upstream_model.clone()), row))
        .collect::<HashMap<_, _>>();
    if include_p95 {
        let p95 = runtime_target_p95_latency(state, targets).await?;
        for (key, value) in p95 {
            if let Some(metric) = metrics.get_mut(&key) {
                metric.p95_latency_ms = Some(value);
            }
        }
    }
    Ok(metrics)
}

/// Nearest-rank P95 for the same one-day success window as the average metric.
///
/// The query only runs for the P95 strategy, so the extra window-function pass
/// never enters the hot path of the other strategies.
async fn runtime_target_p95_latency(
    state: &AppState,
    targets: &[RouteTarget],
) -> AppResult<HashMap<(i64, String), f64>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let provider_ids = targets
        .iter()
        .map(|target| target.provider_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        WITH samples AS (
            SELECT provider_id,
                   upstream_model,
                   CASE
                       WHEN first_token_ms IS NOT NULL AND first_token_ms > 0
                           THEN first_token_ms
                       ELSE latency_ms
                   END AS sample
            FROM usage_logs
            WHERE in_flight = 0
              AND success = 1
              AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')
              AND (first_token_ms > 0 OR latency_ms > 0)
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
        ),
        ranked AS (
            SELECT provider_id,
                   upstream_model,
                   sample,
                   ROW_NUMBER() OVER (
                       PARTITION BY provider_id, upstream_model ORDER BY sample
                   ) AS rank,
                   COUNT(*) OVER (
                       PARTITION BY provider_id, upstream_model
                   ) AS total
            FROM samples
        )
        SELECT provider_id,
               upstream_model,
               CAST(MAX(sample) AS REAL) AS p95_latency_ms
        FROM ranked
        WHERE rank >= (total * 95 + 99) / 100
        GROUP BY provider_id, upstream_model
        "#,
    );
    let rows = query
        .build_query_as::<(i64, String, f64)>()
        .fetch_all(&state.pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(provider_id, model, latency)| ((provider_id, model), latency))
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
pub(super) fn routing_overrides_from_key(
    api_key: Option<&ApiKeyRecord>,
) -> RequestRoutingOverrides {
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
    quota: Option<&crate::quota::QuotaScopeState>,
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
        RouteStrategy::P95LatencyOptimized => match metric.and_then(|metric| metric.p95_latency_ms)
        {
            Some(_) if first => "lowest_recent_p95_latency",
            Some(_) => "recent_p95_latency",
            None => "no_recent_p95_latency",
        },
        RouteStrategy::LeastUsed => match metric {
            Some(_) if first => "least_recent_traffic",
            Some(_) => "recent_traffic",
            None => "no_recent_traffic",
        },
        RouteStrategy::QuotaPaced => match quota.map(|quota| quota.state) {
            Some("healthy") if first => "quota_healthy",
            Some("healthy") => "quota_healthy",
            Some("low") => "quota_low",
            Some("exhausted") => "quota_exhausted",
            Some("reset_due") => "quota_reset_due",
            _ => "quota_unknown",
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
        RouteStrategy::P95LatencyOptimized => {
            targets.sort_by(|left, right| {
                let left_latency = metric_for(left)
                    .and_then(|metric| metric.p95_latency_ms)
                    .unwrap_or(f64::INFINITY);
                let right_latency = metric_for(right)
                    .and_then(|metric| metric.p95_latency_ms)
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
        RouteStrategy::QuotaPaced => {}
        RouteStrategy::Priority | RouteStrategy::Weighted | RouteStrategy::RoundRobin => {}
    }
}

fn quota_reset_at(
    quota: Option<&crate::quota::QuotaScopeState>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    quota
        .and_then(|quota| quota.reset_at.as_deref())
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
}

fn compare_quota_reset(
    left: Option<&crate::quota::QuotaScopeState>,
    right: Option<&crate::quota::QuotaScopeState>,
) -> std::cmp::Ordering {
    match (quota_reset_at(left), quota_reset_at(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn quota_affinity_rank(target: &RouteTarget, affinity: Option<&SessionAffinityContext>) -> u8 {
    match affinity.and_then(|affinity| affinity.pinned.as_ref()) {
        Some(pinned) if same_affinity_target(target, pinned) => 0,
        Some(_) => 1,
        None => 0,
    }
}

fn quota_detail(quota: Option<&crate::quota::QuotaScopeState>) -> String {
    let Some(quota) = quota else {
        return "额度状态未知，按中性值处理".to_string();
    };
    let pressure = quota
        .pressure_pct
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "未知".to_string());
    let reset = quota.reset_at.as_deref().unwrap_or("未知");
    match quota.state {
        "healthy" => format!("已用 {pressure}，{reset} 重置"),
        "low" => format!("已用 {pressure}，接近低水位，{reset} 重置"),
        "exhausted" => format!("额度已耗尽，{reset} 重置"),
        "reset_due" => "重置时间已到，等待下次额度刷新".to_string(),
        _ => "额度状态未知，按中性值处理".to_string(),
    }
}

async fn apply_quota_pacing(
    state: &AppState,
    targets: &mut Vec<RouteTarget>,
    affinity: Option<&SessionAffinityContext>,
) {
    if targets.is_empty() {
        return;
    }
    let quota_states = crate::quota::target_scope_states(state, targets).await;
    let quota_for =
        |target: &RouteTarget| quota_states.get(&crate::quota::quota_target_key(target));
    let now = chrono::Utc::now();
    targets.sort_by(|left, right| {
        let left_quota = quota_for(left);
        let right_quota = quota_for(right);
        crate::quota::quota_pacing_score(right_quota, now)
            .total_cmp(&crate::quota::quota_pacing_score(left_quota, now))
            .then_with(|| compare_quota_reset(left_quota, right_quota))
            .then_with(|| {
                quota_affinity_rank(left, affinity).cmp(&quota_affinity_rank(right, affinity))
            })
            .then_with(|| {
                target_cost_score(left)
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&target_cost_score(right).unwrap_or(f64::INFINITY))
            })
            .then_with(|| left.priority.cmp(&right.priority))
            .then_with(|| left.id.cmp(&right.id))
    });
    if targets
        .iter()
        .any(|target| !quota_for(target).is_some_and(|quota| quota.state == "exhausted"))
    {
        targets.retain(|target| !quota_for(target).is_some_and(|quota| quota.state == "exhausted"));
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
        let keys = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            ),
        >(
            "SELECT id, name, secret, max_concurrency, requests_per_minute, \
                    queue_limit, queue_wait_ms FROM provider_api_keys \
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
            .filter(|(id, _, _, _, _, _, _)| !cooldowns.contains_key(id))
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

        for (
            key_id,
            key_name,
            secret,
            max_concurrency,
            requests_per_minute,
            queue_limit,
            queue_wait_ms,
        ) in keys
        {
            let mut candidate = target.clone();
            candidate.api_key = Some(secret);
            candidate.provider_api_key_id = Some(key_id);
            candidate.provider_api_key_name = Some(key_name);
            candidate.provider_key_max_concurrency = max_concurrency;
            candidate.provider_key_requests_per_minute = requests_per_minute;
            candidate.provider_key_queue_limit = queue_limit;
            candidate.provider_key_queue_wait_ms = queue_wait_ms;
            expanded.push(candidate);
        }
    }

    let last_index = expanded.len().saturating_sub(1);
    for (index, target) in expanded.iter_mut().enumerate() {
        target.auth_retryable = index < last_index;
    }
    Ok(expanded)
}

/// Neutral factor score used when a signal has no data.
///
/// Unknown must never penalise a candidate: a provider that has never served a
/// request, or that publishes no price, would otherwise rank behind a candidate
/// with mediocre known metrics just because less is known about it.
const NEUTRAL_FACTOR_SCORE: f64 = 0.5;

/// One explainable component of a candidate's score.
#[derive(Debug, Clone)]
pub(crate) struct TargetScoreFactor {
    /// Stable factor name (`circuit`, `capacity`, `allocation`, ...).
    pub(crate) name: &'static str,
    /// `0.0..=1.0`, or `None` when nothing is known. Unknown factors contribute
    /// [`NEUTRAL_FACTOR_SCORE`].
    pub(crate) score: Option<f64>,
    pub(crate) weight: f64,
    /// `weight × (score or the neutral value)`, so an operator can see exactly
    /// which factor moved a candidate.
    pub(crate) contribution: f64,
    pub(crate) detail: String,
}

/// Why a candidate ranks where it does.
#[derive(Debug, Clone)]
pub(crate) struct TargetAssessment {
    /// `0..=100`, higher is better; 50 means "no known signal either way".
    pub(crate) score: f64,
    pub(crate) factors: Vec<TargetScoreFactor>,
    /// Set when the request path will not use this candidate at all right now.
    pub(crate) exclusion: Option<String>,
    /// Usable, but tried after candidates without the penalty (a soft pool
    /// share, a failed health check, saturated concurrency, a slow successful
    /// first token, or a cooling circuit that has no healthy alternative).
    pub(crate) deprioritized: bool,
}

impl TargetAssessment {
    fn new(factors: Vec<TargetScoreFactor>) -> Self {
        let mut weight_sum = 0.0;
        let mut score_sum = 0.0;
        for factor in &factors {
            weight_sum += factor.weight;
            score_sum += factor.contribution;
        }
        let score = if weight_sum > 0.0 {
            score_sum / weight_sum * 100.0
        } else {
            NEUTRAL_FACTOR_SCORE * 100.0
        };
        Self {
            score: (score * 100.0).round() / 100.0,
            factors,
            exclusion: None,
            deprioritized: false,
        }
    }

    fn excluded(mut self, reason: String) -> Self {
        self.exclusion = Some(reason);
        self
    }

    fn penalized(mut self) -> Self {
        self.deprioritized = true;
        self
    }
}

fn score_factor(
    name: &'static str,
    weight: f64,
    score: Option<f64>,
    detail: impl Into<String>,
) -> TargetScoreFactor {
    let score = score.map(|value| value.clamp(0.0, 1.0));
    TargetScoreFactor {
        name,
        weight,
        score,
        contribution: weight * score.unwrap_or(NEUTRAL_FACTOR_SCORE),
        detail: detail.into(),
    }
}

/// Seconds left on a monotonic cooldown deadline, rounded up and never below 1
/// so a displayed "remaining" is never a misleading zero while still cooling.
fn cooldown_remaining_seconds(until: Instant, now: Instant) -> u64 {
    let remaining = until.saturating_duration_since(now);
    (remaining.as_secs_f64().ceil() as u64).max(1)
}

/// Fraction of a `(available, limit)` pair that is still free.
fn headroom_ratio(headroom: &(usize, usize)) -> f64 {
    if headroom.1 == 0 {
        1.0
    } else {
        headroom.0 as f64 / headroom.1 as f64
    }
}

/// Concurrency headroom of the worst-limited dimension, as
/// `(available, limit)`; `None` when the target caps nothing.
///
/// A limit that was configured but never used has no semaphore yet, which
/// means every slot is free — not that the target is saturated.
async fn concurrency_headroom(state: &AppState, target: &RouteTarget) -> Option<(usize, usize)> {
    let mut worst: Option<(usize, usize)> = None;
    if let Some(provider_api_key_id) = target.provider_api_key_id {
        if let Some(limit) = target
            .provider_key_max_concurrency
            .filter(|limit| *limit > 0)
        {
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let available = {
                let semaphores = state.provider_key_concurrency.lock().await;
                semaphores
                    .get(&provider_api_key_id)
                    .map(|semaphore| semaphore.available_permits())
                    .unwrap_or(limit)
            };
            let candidate = (available, limit);
            if headroom_ratio(&candidate)
                < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
            {
                worst = Some(candidate);
            }
        }
        if let Some(limit) = target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0)
        {
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let (used, _) = provider_key_rpm_window(state, provider_api_key_id).await;
            let candidate = (limit.saturating_sub(used), limit);
            if headroom_ratio(&candidate)
                < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
            {
                worst = Some(candidate);
            }
        }
    }
    if let Some(limit) = target.max_concurrency.filter(|limit| *limit > 0) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let available = {
            let semaphores = state.provider_concurrency.lock().await;
            semaphores
                .get(&target.provider_id)
                .map(|semaphore| semaphore.available_permits())
                .unwrap_or(limit)
        };
        let candidate = (available, limit);
        if headroom_ratio(&candidate)
            < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
        {
            worst = Some(candidate);
        }
    }
    if let Some(limit) = target.model_max_concurrency.filter(|limit| *limit > 0) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let available = {
            let semaphores = state.model_concurrency.lock().await;
            semaphores
                .get(&(target.provider_id, target.upstream_model.clone()))
                .map(|semaphore| semaphore.available_permits())
                .unwrap_or(limit)
        };
        let candidate = (available, limit);
        if headroom_ratio(&candidate)
            < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
        {
            worst = Some(candidate);
        }
    }
    worst
}

/// Pool headroom for one key, plus a denial reason or a soft penalty.
async fn allocation_signal(
    state: &AppState,
    provider_id: i64,
    api_key_id: Option<i64>,
) -> (Option<TargetScoreFactor>, Option<String>, bool) {
    let Ok(catalog) = state.pool_catalog().await else {
        return (None, None, false);
    };
    let pools = catalog.pools_for_provider(provider_id);
    if pools.is_empty() {
        return (None, None, false);
    }
    let api_key_id = api_key_id.unwrap_or(crate::allocation::ANONYMOUS_API_KEY_ID);
    let now = Instant::now();
    let mut headroom = f64::INFINITY;
    let mut details = Vec::new();
    let mut denied = None;
    let mut penalized = false;
    for spec in pools {
        let decision = {
            let mut ledger = state.allocation_ledger.lock().await;
            ledger.decide(spec, api_key_id, now)
        };
        match decision {
            PoolDecision::Allow => {}
            PoolDecision::Penalize { reason } => {
                penalized = true;
                details.push(reason);
            }
            PoolDecision::Deny { reason, .. } => {
                denied = Some(reason);
            }
        }
        let usage = {
            let mut ledger = state.allocation_ledger.lock().await;
            ledger.usage(spec, api_key_id, now)
        };
        if let Some(limit) = spec.request_limit {
            headroom = headroom.min(limit.saturating_sub(usage.requests) as f64 / limit as f64);
            details.push(format!(
                "池 '{}' 请求余量 {}/{}",
                spec.name,
                limit.saturating_sub(usage.requests),
                limit
            ));
        }
        if let Some(limit) = spec.token_limit {
            headroom = headroom.min(limit.saturating_sub(usage.tokens) as f64 / limit as f64);
            details.push(format!(
                "池 '{}' token 余量 {}/{}",
                spec.name,
                limit.saturating_sub(usage.tokens),
                limit
            ));
        }
    }
    if let Some(reason) = denied {
        return (
            Some(score_factor("allocation", 2.0, Some(0.0), reason.clone())),
            Some(reason),
            penalized,
        );
    }
    let score = headroom.is_finite().then_some(headroom);
    (
        Some(score_factor("allocation", 2.0, score, details.join("；"))),
        None,
        penalized,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct SessionAffinityContext {
    scope: crate::session_affinity::SessionAffinityScope,
    pinned: Option<crate::session_affinity::SessionAffinityTarget>,
    record_bind: bool,
}

async fn session_affinity_context(
    state: &AppState,
    route_id: i64,
    session_key: Option<&str>,
    api_key_id: Option<i64>,
    record_metrics: bool,
) -> Option<SessionAffinityContext> {
    let store = state.session_affinity.as_ref()?;
    let session_key = session_key
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let scope =
        crate::session_affinity::SessionAffinityScope::new(api_key_id, route_id, session_key);
    let pinned = if record_metrics {
        store.lookup(&state.pool, &scope).await
    } else {
        store.peek(&state.pool, &scope).await
    };
    Some(SessionAffinityContext {
        scope,
        pinned,
        record_bind: record_metrics,
    })
}

fn same_affinity_target(
    target: &RouteTarget,
    affinity: &crate::session_affinity::SessionAffinityTarget,
) -> bool {
    target.provider_id == affinity.provider_id && target.upstream_model == affinity.upstream_model
}

fn affinity_target(target: &RouteTarget) -> crate::session_affinity::SessionAffinityTarget {
    crate::session_affinity::SessionAffinityTarget {
        provider_id: target.provider_id,
        upstream_model: target.upstream_model.clone(),
    }
}

fn session_factor(
    context: &SessionAffinityContext,
    target: &RouteTarget,
    cache_metrics: Option<&RuntimeTargetMetricRow>,
) -> TargetScoreFactor {
    let cache_roi = cache_metrics.and_then(RuntimeTargetMetricRow::cache_roi);
    let cache_evidence = cache_roi.is_some();
    let weight = cache_roi.map_or(0.25, |roi| 0.75 + roi * 1.75);
    let cache_detail = cache_metrics
        .zip(cache_roi)
        .map(|(metrics, roi)| {
            format!(
                "，近一天缓存收益估值约 {:.1}%（命中率 {:.1}%，读取 {} / 写入 {} / 输入 {}）",
                roi * 100.0,
                metrics.cache_hit_rate().unwrap_or(0.0) * 100.0,
                metrics.cache_read_tokens,
                metrics.cache_write_tokens,
                metrics.prompt_tokens
            )
        })
        .unwrap_or_default();
    match &context.pinned {
        Some(pinned) if same_affinity_target(target, pinned) => score_factor(
            "session",
            weight,
            Some(1.0),
            if cache_evidence {
                format!(
                    "命中会话粘性与缓存收益{cache_detail}，优先回到 {} / {}",
                    target.provider_name, target.upstream_model,
                )
            } else {
                format!(
                    "命中会话粘性（暂无缓存收益证据），{} / {} 只保留普通优先级",
                    target.provider_name, target.upstream_model
                )
            },
        ),
        Some(pinned) => score_factor(
            "session",
            weight,
            Some(0.0),
            if cache_evidence {
                format!(
                    "本会话已有缓存收益{cache_detail}，优先回到 provider {} / {}",
                    pinned.provider_id, pinned.upstream_model,
                )
            } else {
                format!(
                    "本会话原先在 provider {} / {}，但没有缓存收益证据，不阻塞策略排序",
                    pinned.provider_id, pinned.upstream_model
                )
            },
        ),
        None => score_factor(
            "session",
            0.25,
            None,
            "该会话尚未建立粘性目标；首个健康候选会被记住",
        ),
    }
}

/// Scores every candidate with an explainable factor breakdown.
///
/// Performs no upstream request: every signal comes from the database, the
/// in-memory control state, or recent usage rows.
pub(crate) async fn assess_targets(
    state: &AppState,
    strategy: RouteStrategy,
    targets: &[RouteTarget],
    api_key_id: Option<i64>,
    affinity: Option<&SessionAffinityContext>,
) -> AppResult<HashMap<(i64, String), TargetAssessment>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let metrics = runtime_target_metrics(
        state,
        targets,
        strategy == RouteStrategy::P95LatencyOptimized,
    )
    .await?;
    let quota_states = crate::quota::target_scope_states(state, targets).await;
    let quota_now = chrono::Utc::now();
    let key_of = |target: &RouteTarget| (target.provider_id, target.upstream_model.clone());

    // Relative signals are only meaningful next to the other candidates.
    let cheapest = targets
        .iter()
        .filter_map(target_cost_score)
        .filter(|cost| *cost > 0.0)
        .fold(f64::INFINITY, f64::min);
    let fastest = targets
        .iter()
        .filter_map(|target| {
            metrics
                .get(&key_of(target))
                .and_then(|metric| metric.avg_latency_ms)
        })
        .filter(|latency| *latency > 0.0)
        .fold(f64::INFINITY, f64::min);
    let fastest_p95 = targets
        .iter()
        .filter_map(|target| {
            metrics
                .get(&key_of(target))
                .and_then(|metric| metric.p95_latency_ms)
        })
        .filter(|latency| *latency > 0.0)
        .fold(f64::INFINITY, f64::min);
    let busiest = targets
        .iter()
        .filter_map(|target| metrics.get(&key_of(target)).map(|metric| metric.requests))
        .max()
        .unwrap_or(0);
    let best_priority = targets
        .iter()
        .map(|target| target.priority)
        .min()
        .unwrap_or(0);
    let highest_weight = targets
        .iter()
        .map(|target| target.weight)
        .max()
        .unwrap_or(1);

    let now = Instant::now();
    let threshold = provider_open_threshold();
    let streaks = state.provider_failure_streak.lock().await.clone();
    let provider_cooldowns = {
        let mut cooldowns = state.provider_cooldown.lock().await;
        cooldowns.retain(|_, until| *until + PROVIDER_PROBE_LEASE > now);
        cooldowns.clone()
    };
    let model_cooldowns = {
        let mut cooldowns = state.target_cooldown.lock().await;
        cooldowns.retain(|_, until| *until + PROVIDER_PROBE_LEASE > now);
        cooldowns.clone()
    };
    let slow_targets = {
        let mut targets = state.target_slow.lock().await;
        targets.retain(|_, entry| {
            entry.until.is_some_and(|until| until > now)
                || entry.last_slow_at.is_some_and(|last| {
                    now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW
                })
        });
        targets.clone()
    };
    let target_sinks = {
        let mut sinks = state.target_sink.lock().await;
        sinks.retain(|_, entry| entry.until > now);
        sinks.clone()
    };

    let mut assessments = HashMap::with_capacity(targets.len());
    for target in targets {
        let key = key_of(target);
        let metric = metrics.get(&key);
        let mut factors = Vec::new();
        let target_sink_key = target_sink_key(target);
        let sink_penalized = target_sinks.contains_key(&target_sink_key);
        if sink_penalized {
            factors.push(score_factor(
                "sink",
                1.25,
                Some(0.0),
                "短期限流 sink，仍可重试但排在健康目标之后",
            ));
        }

        // 1. Circuit state: an open provider is short-circuited before any
        //    upstream call, so it must score at the bottom.
        let streak = streaks.get(&target.provider_id).copied().unwrap_or(0);
        let provider_until = provider_cooldowns.get(&target.provider_id).copied();
        let cooling = provider_until.is_some_and(|until| until > now);
        let circuit_state = crate::state::provider_circuit_state(streak, cooling, threshold);
        let (circuit_score, circuit_detail) = match circuit_state {
            "open" => (
                0.0,
                format!(
                    "熔断中（连续失败 {streak} 次，冷却剩余 {}s）",
                    cooldown_remaining_seconds(provider_until.unwrap_or(now), now)
                ),
            ),
            "half_open" => (
                0.5,
                format!("半开，仅放行一个探测请求（连续失败 {streak} 次）"),
            ),
            _ => (1.0, "熔断关闭".to_string()),
        };
        factors.push(score_factor(
            "circuit",
            2.5,
            Some(circuit_score),
            circuit_detail,
        ));

        // 2. Live concurrency capacity.
        let capacity = concurrency_headroom(state, target).await;
        let capacity_score = capacity.map(|headroom| headroom_ratio(&headroom));
        let capacity_detail = match capacity {
            Some((available, limit)) => format!("并发余量 {available}/{limit}"),
            None => "未设置并发上限".to_string(),
        };
        let saturated = capacity.is_some_and(|(available, _)| available == 0);
        factors.push(score_factor(
            "capacity",
            2.0,
            capacity_score,
            capacity_detail,
        ));

        // 3. Pool allocation: denial is a hard exclusion, a soft share only
        //    deprioritises.
        let (allocation_factor, allocation_denied, allocation_penalized) =
            allocation_signal(state, target.provider_id, api_key_id).await;

        // 4. Provider health from the last test or scheduled probe.
        let (health_score, health_detail, health_failed) = match target.provider_health {
            Some(0) => (0.0, "最近一次健康检查失败".to_string(), true),
            Some(_) => (1.0, "最近一次健康检查通过".to_string(), false),
            None => (NEUTRAL_FACTOR_SCORE, "尚未做过健康检查".to_string(), false),
        };
        factors.push(score_factor(
            "health",
            2.5,
            Some(health_score),
            health_detail,
        ));

        // 5. Recent upstream-caused success rate.
        let reliability = metric.and_then(|metric| metric.success_rate());
        let reliability_detail = match (metric, reliability) {
            (Some(metric), Some(rate)) => format!(
                "近期成功率 {:.1}%（{}/{} 次上游结果）",
                rate * 100.0,
                metric.successes,
                metric.successes + metric.upstream_faults
            ),
            _ => "近期没有可判定的上游结果".to_string(),
        };
        factors.push(score_factor(
            "reliability",
            2.0,
            reliability,
            reliability_detail,
        ));

        // 6. Latency relative to the fastest known candidate.
        let latency = metric
            .and_then(|metric| metric.avg_latency_ms)
            .filter(|value| *value > 0.0);
        let latency_score = latency.map(|latency| {
            if fastest.is_finite() && fastest > 0.0 {
                (fastest / latency).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let latency_detail = match latency {
            Some(latency) => format!("近期平均 {latency:.0}ms（最快 {fastest:.0}ms）"),
            None => "近期没有延迟样本".to_string(),
        };
        factors.push(score_factor(
            "latency",
            1.0,
            latency_score,
            latency_detail.clone(),
        ));
        let p95_latency = metric
            .and_then(|metric| metric.p95_latency_ms)
            .filter(|value| *value > 0.0);
        let p95_latency_score = p95_latency.map(|latency| {
            if fastest_p95.is_finite() && fastest_p95 > 0.0 {
                (fastest_p95 / latency).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let p95_latency_detail = match p95_latency {
            Some(latency) => format!("近期 P95 {latency:.0}ms（最快 {fastest_p95:.0}ms）"),
            None => "近期没有 P95 延迟样本".to_string(),
        };

        // Only the latency-first strategy lets a recent slow first token
        // reshuffle the order; every other strategy keeps the order the
        // operator configured.
        let slow_state = slow_targets.get(&key).filter(|entry| {
            strategy.is_latency_first() && entry.until.is_some_and(|until| until > now)
        });
        if let Some(slow_state) = slow_state {
            factors.push(score_factor(
                "first_token",
                1.5,
                Some(0.0),
                format!(
                    "最近一次首字 {}ms，软降权剩余 {}s",
                    slow_state.last_first_token_ms,
                    cooldown_remaining_seconds(slow_state.until.unwrap_or(now), now)
                ),
            ));
        }

        // 7. Cost relative to the cheapest known candidate.
        let cost = target_cost_score(target);
        let cost_score = cost.map(|cost| {
            if cheapest.is_finite() && cheapest > 0.0 {
                (cheapest / cost).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let cost_detail = match cost {
            Some(cost) => format!("成本指数 {cost:.4}（最低 {cheapest:.4}）"),
            None => "未公布价格".to_string(),
        };
        factors.push(score_factor("cost", 1.0, cost_score, cost_detail.clone()));

        let quota = quota_states.get(&crate::quota::quota_target_key(target));
        let quota_score = crate::quota::quota_pacing_score(quota, quota_now);
        let quota_detail = quota_detail(quota);
        let quota_low = quota.is_some_and(|quota| quota.state == "low");
        let quota_exhausted = quota.is_some_and(|quota| quota.state == "exhausted");
        let quota_alternative = targets.iter().any(|other| {
            !quota_states
                .get(&crate::quota::quota_target_key(other))
                .is_some_and(|quota| quota.state == "exhausted")
        });
        if strategy == RouteStrategy::QuotaPaced {
            factors.push(score_factor(
                "quota",
                2.0,
                Some(quota_score),
                quota_detail.clone(),
            ));
        }

        // 8. The strategy's own preference, so the score agrees with the order
        //    the request path will use.
        let (strategy_score, strategy_detail) = match strategy {
            RouteStrategy::Priority => {
                let rank = target.priority.saturating_sub(best_priority);
                (
                    Some(1.0 / (1.0 + rank as f64)),
                    format!("优先级 {}（最低 {best_priority}）", target.priority),
                )
            }
            RouteStrategy::Weighted => (
                Some(target.weight.max(0) as f64 / highest_weight.max(1) as f64),
                format!("权重 {}（最高 {highest_weight}）", target.weight),
            ),
            RouteStrategy::RoundRobin => (Some(1.0), "按轮询游标顺序".to_string()),
            RouteStrategy::CostOptimized => (cost_score, cost_detail.clone()),
            RouteStrategy::LatencyOptimized => (latency_score, latency_detail.clone()),
            RouteStrategy::P95LatencyOptimized => (p95_latency_score, p95_latency_detail.clone()),
            RouteStrategy::LeastUsed => {
                let requests = metric.map(|metric| metric.requests).unwrap_or(0);
                (
                    Some(1.0 - requests as f64 / busiest.max(1) as f64),
                    format!("近期 {requests} 次请求（最多 {busiest} 次）"),
                )
            }
            RouteStrategy::QuotaPaced => (Some(quota_score), quota_detail),
        };
        factors.push(score_factor(
            "strategy",
            1.5,
            strategy_score,
            strategy_detail,
        ));
        if let Some(affinity) = affinity {
            let pinned_metrics = affinity.pinned.as_ref().and_then(|pinned| {
                metrics.get(&(pinned.provider_id, pinned.upstream_model.clone()))
            });
            factors.push(session_factor(affinity, target, pinned_metrics));
        }

        let mut assessment = TargetAssessment::new(factors);
        if health_failed
            || saturated
            || allocation_penalized
            || slow_state.is_some()
            || quota_low
            || sink_penalized
        {
            assessment = assessment.penalized();
        }

        // A model cooling down is skipped while a healthy alternative exists,
        // an open circuit is short-circuited before the upstream call, and a
        // pool denial removes the candidate outright.
        let model_until = model_cooldowns
            .get(&(target.provider_id, target.upstream_model.clone()))
            .copied();
        let model_cooling = model_until.is_some_and(|until| until > now);
        let sibling_exists = targets.iter().any(|other| {
            other.provider_id != target.provider_id || other.upstream_model != target.upstream_model
        });
        if let Some(reason) = allocation_denied {
            assessment = assessment.excluded(format!("提供商配额池拒绝：{reason}"));
        } else if circuit_state == "open" {
            assessment = assessment.excluded("提供商熔断中，请求会被立即短路".to_string());
        } else if model_cooling && sibling_exists {
            assessment = assessment.excluded(format!(
                "该模型正在冷却（剩余 {}s），存在可用替代目标",
                cooldown_remaining_seconds(model_until.unwrap_or(now), now)
            ));
        } else if strategy == RouteStrategy::QuotaPaced && quota_exhausted && quota_alternative {
            assessment = assessment.excluded("额度已耗尽，存在可用替代目标".to_string());
        }
        if let Some(factor) = allocation_factor {
            // Keep the factor list in a stable order for the UI.
            let mut factors = assessment.factors;
            factors.insert(3, factor);
            assessment.factors = factors;
        }
        assessments.insert(key, assessment);
    }
    Ok(assessments)
}

/// Candidates that are usable but should be tried last.
///
/// This is the SQL-free subset of [`assess_targets`] used on the request path:
/// it only reads in-memory control state (concurrency slots, pool window,
/// circuit/cooldown maps), so it adds no query to a normal request. The
/// recent first-token signal is folded in only for `latency_optimized` routes;
/// every other strategy keeps its own configured order.
pub(super) async fn deprioritized_target_keys(
    state: &AppState,
    strategy: RouteStrategy,
    targets: &[RouteTarget],
    api_key_id: Option<i64>,
) -> HashMap<(i64, Option<i64>, String), &'static str> {
    let mut keys = HashMap::new();
    if targets.is_empty() {
        return keys;
    }
    let now = Instant::now();
    let threshold = provider_open_threshold();
    let streaks = state.provider_failure_streak.lock().await.clone();
    let provider_cooldowns = state.provider_cooldown.lock().await.clone();
    let target_sinks = {
        let mut sinks = state.target_sink.lock().await;
        sinks.retain(|_, entry| entry.until > now);
        sinks.clone()
    };
    let slow_targets = if strategy.is_latency_first() {
        let mut targets = state.target_slow.lock().await;
        targets.retain(|_, entry| {
            entry.until.is_some_and(|until| until > now)
                || entry.last_slow_at.is_some_and(|last| {
                    now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW
                })
        });
        targets
            .iter()
            .filter(|(_, entry)| entry.until.is_some_and(|until| until > now))
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    for target in targets {
        let slow_key = (target.provider_id, target.upstream_model.clone());
        let target_key = (
            target.provider_id,
            target.provider_api_key_id,
            target.upstream_model.clone(),
        );
        if target_sinks.contains_key(&target_key) {
            keys.insert(target_key, "target_sink");
            continue;
        }
        if slow_targets.contains(&slow_key) {
            keys.insert(target_key, "slow_first_token");
            continue;
        }
        let streak = streaks.get(&target.provider_id).copied().unwrap_or(0);
        let cooling = provider_cooldowns
            .get(&target.provider_id)
            .is_some_and(|until| *until > now);
        if crate::state::provider_circuit_state(streak, cooling, threshold) == "open" {
            keys.insert(target_key, "provider_circuit_open");
            continue;
        }
        if target.provider_health == Some(0) {
            keys.insert(target_key, "provider_health_failed");
            continue;
        }
        if concurrency_headroom(state, target)
            .await
            .is_some_and(|(available, _)| available == 0)
        {
            keys.insert(target_key, "concurrency_saturated");
            continue;
        }
        let Ok(catalog) = state.pool_catalog().await else {
            continue;
        };
        let pools = catalog.pools_for_provider(target.provider_id);
        if pools.is_empty() {
            continue;
        }
        // The real gateway key is used (not the anonymous probe): a `soft`
        // allocation penalises the key that is over its own share, and using
        // the anonymous id here would leave that key unpenalised on the real
        // request path while the diagnose preview did penalise it.
        let api_key_id = api_key_id.unwrap_or(crate::allocation::ANONYMOUS_API_KEY_ID);
        let penalized = {
            let mut ledger = state.allocation_ledger.lock().await;
            pools.iter().any(|spec| {
                matches!(
                    ledger.decide(spec, api_key_id, now),
                    PoolDecision::Penalize { .. }
                )
            })
        };
        if penalized {
            keys.insert(target_key, "pool_share");
        }
    }
    keys
}

/// Turns an assessment into the API-facing candidate row.
pub(crate) fn candidate_view(
    order: usize,
    target: &RouteTarget,
    assessment: &TargetAssessment,
) -> RouteDiagnoseCandidate {
    RouteDiagnoseCandidate {
        order,
        provider_id: target.provider_id,
        provider_name: target.provider_name.clone(),
        upstream_model: target.upstream_model.clone(),
        score: assessment.score,
        eligible: assessment.exclusion.is_none(),
        exclusion: assessment.exclusion.clone(),
        deprioritized: assessment.deprioritized,
        failure_scope: None,
        failure_retry: None,
        failure_confidence: None,
        failure_reason_code: None,
        failure_observed_seconds_ago: None,
        factors: assessment
            .factors
            .iter()
            .map(|factor| RouteDiagnoseFactor {
                name: factor.name.to_string(),
                score: factor.score,
                weight: factor.weight,
                contribution: factor.contribution,
                detail: factor.detail.clone(),
            })
            .collect(),
    }
}

pub(super) struct OrderedTargets {
    pub(super) targets: Vec<RouteTarget>,
    pub(super) session_decision: &'static str,
}

#[cfg(test)]
pub(super) async fn order_targets(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<Vec<RouteTarget>> {
    Ok(
        order_targets_with_decision(state, route_id, strategy, targets, session_id, api_key_id)
            .await?
            .targets,
    )
}

pub(super) async fn order_targets_with_decision(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<OrderedTargets> {
    let affinity = session_affinity_context(state, route_id, session_id, api_key_id, true).await;
    order_targets_with_context_result(
        state,
        route_id,
        strategy,
        targets,
        session_id,
        api_key_id,
        affinity.as_ref(),
    )
    .await
}

/// Compatibility wrapper for diagnostic callers that only need the order.
async fn order_targets_with_context(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
    affinity: Option<&SessionAffinityContext>,
) -> AppResult<Vec<RouteTarget>> {
    Ok(order_targets_with_context_result(
        state, route_id, strategy, targets, session_id, api_key_id, affinity,
    )
    .await?
    .targets)
}

async fn order_targets_with_context_result(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    mut targets: Vec<RouteTarget>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
    affinity: Option<&SessionAffinityContext>,
) -> AppResult<OrderedTargets> {
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let session_id = session_id.map(str::trim).filter(|value| !value.is_empty());
    let cache_aware = !matches!(
        strategy,
        RouteStrategy::Priority | RouteStrategy::Weighted | RouteStrategy::RoundRobin
    );
    let needs_runtime_metrics = matches!(
        strategy,
        RouteStrategy::LatencyOptimized
            | RouteStrategy::P95LatencyOptimized
            | RouteStrategy::LeastUsed
    ) || (cache_aware
        && affinity
            .and_then(|context| context.pinned.as_ref())
            .is_some());
    let runtime_metrics = if needs_runtime_metrics {
        runtime_target_metrics(
            state,
            &targets,
            strategy == RouteStrategy::P95LatencyOptimized,
        )
        .await?
    } else {
        HashMap::new()
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
            | RouteStrategy::P95LatencyOptimized
            | RouteStrategy::LeastUsed
            | RouteStrategy::QuotaPaced => {}
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

    let mut targets = expand_target_provider_keys(state, targets, session_id).await?;

    // Try soft-degraded candidates last even when their last health probe
    // passed: a successful response that took too long is newer evidence than
    // an older health check. Within each soft bucket, unknown health stays
    // ahead of explicitly failed providers so a previously-tested outage does
    // not permanently suppress a recovery. The stable sort keeps the
    // strategy's own order inside each bucket.
    let deprioritized = deprioritized_target_keys(state, strategy, &targets, api_key_id).await;
    targets.sort_by_key(|target| {
        (
            u8::from(deprioritized.contains_key(&(
                target.provider_id,
                target.provider_api_key_id,
                target.upstream_model.clone(),
            ))),
            provider_health_rank(target.provider_health),
        )
    });
    if let Some(affinity) = affinity {
        let pinned_quota_impaired = if let Some(pinned) = affinity.pinned.as_ref() {
            let quota_states = crate::quota::target_scope_states(state, &targets).await;
            let mut matching = targets
                .iter()
                .filter(|target| same_affinity_target(target, pinned))
                .peekable();
            matching.peek().is_some()
                && matching.all(|target| {
                    quota_states
                        .get(&crate::quota::quota_target_key(target))
                        .is_some_and(|quota| {
                            quota.state == "low"
                                || quota.state == "exhausted"
                                || quota.state == "reset_due"
                        })
                })
        } else {
            false
        };
        let pinned_has_cache = affinity.pinned.as_ref().is_some_and(|pinned| {
            runtime_metrics
                .get(&(pinned.provider_id, pinned.upstream_model.clone()))
                .is_some_and(RuntimeTargetMetricRow::has_cache_evidence)
        });
        let pinned_index = affinity.pinned.as_ref().and_then(|pinned| {
            targets.iter().position(|target| {
                same_affinity_target(target, pinned)
                    && !deprioritized.contains_key(&(
                        target.provider_id,
                        target.provider_api_key_id,
                        target.upstream_model.clone(),
                    ))
            })
        });
        if let Some(index) = pinned_index {
            if !pinned_quota_impaired && (!cache_aware || pinned_has_cache) && index > 0 {
                targets[..=index].rotate_right(1);
            }
        } else if affinity.record_bind {
            let healthy_index = targets.iter().position(|target| {
                !deprioritized.contains_key(&(
                    target.provider_id,
                    target.provider_api_key_id,
                    target.upstream_model.clone(),
                ))
            });
            if let (Some(index), Some(store)) = (healthy_index, state.session_affinity.as_ref()) {
                let target = affinity_target(&targets[index]);
                if affinity.pinned.as_ref() != Some(&target) {
                    store.bind(&state.pool, &affinity.scope, target).await;
                }
            }
        }
    }
    if strategy == RouteStrategy::QuotaPaced {
        apply_quota_pacing(state, &mut targets, affinity).await;
    }
    let session_decision = match affinity.and_then(|context| context.pinned.as_ref()) {
        Some(pinned)
            if targets
                .first()
                .is_some_and(|target| same_affinity_target(target, pinned)) =>
        {
            "hit"
        }
        Some(_) => "miss",
        None if affinity.is_some() => "miss",
        None => "no_affinity",
    };
    Ok(OrderedTargets {
        targets,
        session_decision,
    })
}
