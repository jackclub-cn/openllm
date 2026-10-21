use super::*;

pub(super) fn target_decision_reason(
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

pub(super) fn sort_targets_by_strategy_metrics(
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

pub(super) fn quota_reset_at(
    quota: Option<&crate::quota::QuotaScopeState>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    quota
        .and_then(|quota| quota.reset_at.as_deref())
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
}

pub(super) fn compare_quota_reset(
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

pub(super) fn quota_affinity_rank(
    target: &RouteTarget,
    affinity: Option<&SessionAffinityContext>,
) -> u8 {
    match affinity.and_then(|affinity| affinity.pinned.as_ref()) {
        Some(pinned) if same_affinity_target(target, pinned) => 0,
        Some(_) => 1,
        None => 0,
    }
}

pub(super) fn quota_detail(quota: Option<&crate::quota::QuotaScopeState>) -> String {
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

pub(super) async fn apply_quota_pacing(
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

/// Candidates that are usable but should be tried last.
///
/// This is the SQL-free subset of [`assess_targets`] used on the request path:
/// it only reads in-memory control state (concurrency slots, pool window,
/// circuit/cooldown maps), so it adds no query to a normal request. The
/// recent first-token signal is folded in only for `latency_optimized` routes;
/// every other strategy keeps its own configured order.
pub(in crate::proxy) async fn deprioritized_target_keys(
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

pub(in crate::proxy) struct OrderedTargets {
    pub(in crate::proxy) targets: Vec<RouteTarget>,
    pub(in crate::proxy) session_decision: &'static str,
}

#[cfg(test)]
pub(in crate::proxy) async fn order_targets(
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

pub(in crate::proxy) async fn order_targets_with_decision(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
) -> AppResult<OrderedTargets> {
    let parsed_strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let affinity = session_affinity_context(
        state,
        route_id,
        parsed_strategy,
        session_id,
        api_key_id,
        true,
    )
    .await;
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
pub(super) async fn order_targets_with_context(
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

pub(super) async fn order_targets_with_context_result(
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
