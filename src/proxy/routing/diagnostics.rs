use super::*;

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

pub(in crate::proxy) async fn diagnose_explicit_route(
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

pub(in crate::proxy) async fn diagnose_direct_route(
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
pub(in crate::proxy) async fn build_direct_diagnosis(
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
pub(in crate::proxy) struct DiagnosticTargetRow {
    pub(super) provider_id: i64,
    pub(super) provider_name: String,
    pub(super) provider_type: String,
    pub(super) target_id: i64,
    pub(super) upstream_model: String,
    pub(super) target_weight: i64,
    pub(super) target_priority: i64,
    pub(super) target_enabled: i64,
    pub(super) provider_enabled: i64,
    pub(super) model_exists: i64,
    pub(super) model_enabled: i64,
    pub(super) supported_endpoints: Option<String>,
    pub(super) cost: Option<String>,
    pub(super) cost_input_override: Option<f64>,
    pub(super) cost_output_override: Option<f64>,
    pub(super) cost_cache_read_override: Option<f64>,
    pub(super) cost_cache_write_override: Option<f64>,
    pub(super) provider_health: Option<i64>,
    pub(super) max_concurrency: Option<i64>,
    pub(super) model_max_concurrency: Option<i64>,
}

impl DiagnosticTargetRow {
    pub(super) fn diagnose(
        &self,
        endpoint: &str,
        route_enabled: bool,
    ) -> (RouteDiagnoseTarget, bool) {
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
pub(super) fn diagnostic_route_target(
    row: &DiagnosticTargetRow,
    route_id: Option<i64>,
) -> RouteTarget {
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
        reasoning_efforts: None,
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
pub(in crate::proxy) async fn diagnostic_candidates(
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
    let affinity =
        session_affinity_context(state, route_id, strategy, session_id, api_key_id, false).await;
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
pub(in crate::proxy) async fn diagnostic_runtime_targets(
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
    let affinity = session_affinity_context(
        state,
        route_id,
        strategy,
        Some(session_id),
        api_key_id,
        false,
    )
    .await;
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
